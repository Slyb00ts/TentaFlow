# Plan implementacji — Metryki wydajności i użycia modeli

Realizacja mockupu `model-metrics-20260630` na żywych danych. Cel: analityka użycia
i WYDAJNOŚCI modeli **mesh-wide** (z każdego node'a widać całą sieć), reużywając
istniejący wzorzec replikacji `token_usage_daily`. Nic bokiem przez bazę — wszystko
przez protokół binarny + dashboard.

## Zasady architektury (z tej sesji)
- **Rollup replikowany, single-writer-per-row** — dokładnie jak `token_usage_daily`
  (`sync/core_registry.rs:401`): każdy node pisze tylko swoje wiersze (`node_id`),
  ale replikują się do wszystkich → każdy sumuje całą sieć z lokalnego SQLite. Zero
  forwardowania zapytań.
- **Histogramy zamiast średnich** — percentyle (p50/p90/p99) muszą być addytywne
  między nodami: sumujemy kubełki histogramu, potem liczymy percentyl. Uśrednianie
  percentyli jest matematycznie błędne.
- **Granularność godzinowa** — ogranicza liczbę wierszy; per-request detail zostaje
  lokalnie w `compliance_ai_events` (drill-down on-demand).
- **Wymiar grupy rozwijany przy ODCZYCIE** (join user→group z RBAC) — nie w rollupie,
  bo user może zmienić grupę.

---

> **Poprawki po codex review (5×P1)** — wcielone niżej: pełna mechanika sync (nie sam
> core_registry); punkt zbierania to executor, nie AiGateway (brak tam perf/service/backend/
> modality/queue); klucz musi mieć `node_id`+stabilny wymiar serwisu (service_id jest node-lokalny);
> histogramy z `*_sample_count` (odróżnić brak-pomiaru od 0); Chunk 5 wymaga schematu per-request
> (compliance_ai_events nie ma ttft/decode/modality/queue). Plus retencja, migracja ról, error_count
> w finish_failed, pełna macierz testów.

## CHUNK 1 — Schema + rollup + PEŁNA mechanika sync (fundament backendu)
Pliki: `db/migrations.rs`, `db/models.rs`, `db/repository.rs`, `sync/core_registry.rs`,
`sync/core_materializer.rs`, `mesh/pipeline.rs`.

**[P1] Sama rejestracja w core_registry NIE wystarcza.** `token_usage_daily` działa dzięki
osobnej ścieżce — trzeba odtworzyć KOMPLET (wzór: repository.rs:9360 flush, pipeline.rs:3569
flusher, core_materializer.rs:1822 materializacja):
- `model_metrics_changed_fields()` (BTreeMap pól do Sync Ledger),
- `flush_model_metrics_captures()` — flusher wybiera TYLKO własne wiersze (`node_id = self`)
  i emituje CAŁE wartości (nie deltę),
- spawn flushera w pipeline (jak token usage),
- baseline export (nowe nody dostają snapshot),
- materializator na peerze: ZASTĘPUJE cały zestaw pól (właściciel = jedyny writer, brak
  dodawania), 
- test „nie re-publikujemy cudzych wierszy" (echo replikacji).
Klucz `id` deterministyczny MUSI zawierać wszystkie wymiary + `node_id` (właściciela).
Dodaj `histogram_version` (do wiersza/klucza) — pozwala ewoluować krawędzie kubełków.

**Migracja `model_metrics_rollup`** (godzinowa):
- Klucz: `id` (deterministyczny hash z WSZYSTKICH wymiarów + node_id), `node_id`,
  `org_id`, `user_id`, `model_id`, **`service_key`** (stabilny: `engine_id`/`deployment_id`
  lub `service_name` — NIE surowy `service_id`, bo jest node-lokalny/namespace docelowego
  node'a → [P1] sklejałby różne serwisy), `backend`, `modality`, `hour_bucket`, `histogram_version`.
- Liczniki: `request_count`, `success_count`, `error_count`.
- Tokeny: `prompt_tokens`, `completion_tokens`, `total_tokens`, `embedding_tokens`,
  `audio_ms`, `images`.
- Czas pracy: `prefill_secs_sum`, `decode_secs_sum`, `e2e_latency_ms_sum`, `queue_ms_sum`.
- Histogramy (kolumny per kubełek, addytywne) — **[P1] każdy z własnym `*_sample_count`;
  kubełek inkrementowany TYLKO dla znanego pomiaru (perf=None / GenPerf domyślne 0 NIE trafia
  do kubełka 0 — inaczej percentyle fałszywie niskie):**
  - `ttft_b0..b9` + `ttft_sample_count` (krawędzie ms: 0,50,100,200,400,800,1600,3200,6400,∞)
  - `decode_tps_b0..b7` + `decode_tps_sample_count` (0,10,20,40,80,160,320,∞)
  - `e2e_b0..b9` + `e2e_sample_count` (0,100,250,500,1000,2000,4000,8000,16000,∞)

**Migracja `model_pricing`**: `model_id`, `prompt_per_1k`, `completion_per_1k`,
`audio_per_min`, `image_each`, `updated_at`.
**Uwaga (user 2026-07-01): PUNKT WEJŚCIA cennika = wizard deployu serwisu** (lokalne vLLM/
llama.cpp ORAZ zewnętrzne „claudowe" modele API), nie osobny edytor. Tabela `model_pricing`
to magazyn; zapis następuje przy deployu/konfiguracji modelu — patrz Chunk 6. Ekran Rozliczenia
(m06) tylko POKAZUJE + koryguje wtórnie.

**Repository**: `bump_model_metrics_rollup(...)` — UPSERT (ON CONFLICT dodaje liczniki
+ inkrementuje właściwy kubełek histogramu); `list_model_metrics_rollup(filters)`;
`model_pricing` get/upsert.

**Sync** (`core_registry.rs`): `model_metrics_rollup` jako
`CoreSyncResourceKind::ModelMetricsRollup` (Organization scope, Durable, single-writer-
per-row — kopiuj deskryptor `TokenUsageDaily`); `model_pricing` jako LWW (kopiuj `TokenQuota`).
Test `runtime_tables_are_not_core_synced` bez zmian (rollup JEST synced).

Codex review → Chunk 2.

## CHUNK 2 — Zbieranie metryk (wpięcie w inferencję)
Pliki: `services/runtime/executor.rs` (GŁÓWNE — nie AiGateway), `compliance/ai_gateway.rs`
(rozszerzenie kontraktu jeśli trzeba), `services/runtime/target.rs`.

**[P1] AiGateway NIE ma potrzebnych danych.** `AiEventHandle`/`AiGatewayContext` trzymają
tylko node/org/user/model/quota (ai_gateway.rs:42,79) — brak `service_id`, realnego `backend`,
`modality`, `queue_ms`, czasu faz. `finish_stream_success` przyjmuje text/usage/tool_calls
BEZ perf; non-streaming `ChatCompletionResponse` w ogóle nie ma `perf`. Dlatego:
- **Podchunk 2a — propagacja metadanych**: bump robimy w `ModelRuntimeExecutor` (tam znany
  `ResolvedExecutionTarget` → node_id + service + backend + modality) i tam mamy `GenPerf`
  (total_ms/ttft/decode/prefill) + `Usage`. Alternatywnie rozszerz kontrakt start/finish
  AiGateway o te pola — ale executor jest naturalniejszy.
- **Podchunk 2b — bump**: `bump_model_metrics_rollup()` z perf + tokeny + e2e (koniec−dispatch)
  + queue_ms (admission→dispatch, jeśli mierzalne). Kubełek liczony tylko dla znanych pomiarów.
- **[P1] error_count w finish_FAILED**, nie tylko success — request z błędem bumpuje
  `error_count` + `request_count`, bez próbek perf.
- `service_id` node-lokalny → mapuj na stabilny `service_key` (engine/deployment/nazwa).
- Rollup pisze się na node'ie OBSŁUGUJĄCYM request (single-writer, jego `node_id`) → replika.

Codex review → Chunk 3.

## CHUNK 3 — Protokół + dispatch (API zapytań)
Pliki: `tentaflow-protocol/src/` (nowy `model_metrics.rs`), `message_body.rs`,
`tentaflow-protocol-wasm/src/lib.rs` (+ regen glue), `dispatch/` (nowy `model_metrics.rs`).

- `MessageBody::ModelMetricsBody(ModelMetricsPayload)`:
  - `SummaryRequest { period, period_key, group_by (user|group|model|node|service|day), filters }`
  - `SummaryResponse { rows: Vec<MetricsRowWire> }` — rows niosą sumy tokenów/zapytań/
    kosztu ORAZ percentyle (p50/p90/p99 ttft, decode_tps, e2e) policzone SERWEROWO
    z zsumowanych histogramów.
  - `NodeServiceRequest/Response` (per-node-per-serwis: throughput, obłożenie, error%).
  - `PricingGet/PricingSet`.
- `dispatch/model_metrics.rs`: agreguje z lokalnego `model_metrics_rollup` (= mesh-wide
  dzięki replikacji), grupuje, liczy percentyle z sum kubełków, dolicza koszt z
  `model_pricing`. Uprawnienie `metrics.read` (write dla pricing: `metrics.write`).
- Grupa: join `user_id → group` z RBAC przy budowie odpowiedzi dla `group_by=group`.
- **Regen wasm glue** po zmianie protokołu (lib.rs set fields + rebuild — jak `total_ms`).

Codex review → Chunk 4.

## CHUNK 4 — Ekrany dashboardu (frontend)
Pliki: `www/js/modules/model-metrics.js` (nowy), nav + i18n, `www/css/` jeśli trzeba.

- 6 ekranów 1:1 z mockupu przez `ApiBinary` + komponenty `tf-*`, wykresy jako inline SVG:
  - **Przegląd** (m01): KPI mesh-wide, wykres tokenów w czasie, top modele/userzy/nody.
  - **Userzy i grupy** (m02): tabela userów + rollup grup (join), drill-down usera.
  - **Modele** (m03): per-model + porównanie model×node×backend.
  - **Nody i serwisy** (m04): produkcja, GPU-h, throughput, obłożenie, per-serwis.
  - **Rozliczenia** (m06): koszty wg user/grupa/sprzęt, cennik edytowalny, eksport CSV.
- Pozycja w nav (admin), gate `metrics.read`. Zero raw `<button>/<input>` — tylko `tf-*`.

Codex review → Chunk 5.

## CHUNK 5 — Eksplorator zapytań (drill-down request-level)
Pliki: `db/migrations.rs` (schema per-request), `db/repository.rs`, `dispatch/model_metrics.rs`,
`mesh/` (on-demand), `www/js/modules/model-metrics.js`.

**[P1] `compliance_ai_events` jest za płytkie** — ma tylko `model_id/backend/started_at/
finished_at/status` (migrations.rs:5937), BRAK `service_key/modality/queue_ms/ttft_ms/
decode_tps/total_ms/prefill_tps` ani faz timeline. Trzeba dodać per-request metryki:
- Nowa LOKALNA tabela `model_request_metrics` (runtime, NIE synced — jak flow_executions)
  z pełnym zestawem per-request (fazy: admission/prefill/decode/total, tokeny, service_key,
  modality) — pisana w tym samym miejscu co bump rollupu (Chunk 2).
- Ekran **Eksplorator** (m05): per-request z tej tabeli (lokalne na node'ie-właścicielu).
  Cross-node = on-demand `MeshCommandType::MetricsRequestDetail` do node'a-właściciela.
  Timeline faz zapytania.

Codex review → koniec.

## CHUNK 6 — Cennik w wizardzie deployu (punkt wejścia kosztów)
Pliki: deploy wizard/form (lokalne serwisy: `www/js/modules/services*.js` / deploy modal;
distributed: `cluster-detail.js`), konfiguracja zewnętrznych „claudowych" modeli
(zewnętrzne connectory LLM — do zlokalizowania: Settings → Dostępy zewnętrzne / connector config),
protokół deployu (przekazać pricing), `dispatch/` + `repository::upsert_model_pricing`.

- **Lokalne modele** (vLLM/llama.cpp): do formularza deployu serwisu LLM dodać opcjonalne pola
  cennika (prompt/1k, completion/1k, audio/min, obraz) — zapisywane do `model_pricing` przy deployu.
- **Zewnętrzne „claudowe" modele** (Claude/OpenAI-compatible connectory): przy konfiguracji
  modelu/connectora te same pola cennika → `model_pricing`.
- Walidacja: pola opcjonalne (brak = koszt 0/nieznany, UI oznacza „brak cennika").
- Ekran Rozliczenia (m06) czyta `model_pricing`; korekta tam jest wtórna (nadpisuje LWW).
- **[decyzja do potwierdzenia w tym chunku]**: gdzie dokładnie w GUI konfiguruje się modele
  zewnętrzne „claudowe" (żeby wpiąć pola cennika we właściwy formularz).

Codex review → koniec.

## PRZEKROJOWE (do każdego chunku)
- **[P1] Retencja/kompakcja** (od Chunku 1, nie na końcu): kardynalność `node×org×user×model×
  service×backend×modality×hour` może eksplodować (flow/alias fallbacki rozszczepiają model_id/
  service). Plan: godzinowe 30-90 dni → potem dzienne agregaty bez `user_id`/`service_key`.
  Zadanie kompakcji w schedulerze. Bez tego Sync Ledger pompuje dużo Durable danych.
- **[P2] Uprawnienia**: migracja ról `metrics.read`/`metrics.write` (jak `tokens.read/write`),
  handler sprawdza permisje (wzór dispatch/token_usage.rs:19), nav-hiding, testy allow/deny.
- **Backfill**: świadoma decyzja — metryki OD ZERA (nie backfillujemy z token_usage_daily,
  bo brak tam perf/histogramów). Do potwierdzenia.
- **Protokół**: nowy wariant enuma `MessageBody` TYLKO na końcu (ostrzeżenie message_body.rs:7110),
  regen wasm glue jako punkt „CI".

## MACIERZ TESTÓW (wymagane)
- sync 2 nodów: sumowanie histogramów + „nie re-publikuj cudzych wierszy" (echo).
- percentyle p50/p90/p99 z BRAKUJĄCYMI próbkami (perf=None nie zaniża).
- kardynalność/retencja/kompakcja.
- streaming z `include_usage=false` (brak usage), non-streaming bez perf.
- mesh-forward z lokalnym `service_id` (mapowanie na service_key).
- `error_count` bumpowany w finish_failed.
- migracje wsteczne/idempotencja na istniejącym DB.

---

## Kolejność / zależności
`1 (schema) → 2 (zbieranie) → 3 (protokół+API) → 4 (ekrany) → 5 (drill-down)`.
Chunk N+1 zależy od N. Codex review po każdym chunku (zasada projektu).

## Otwarte decyzje do potwierdzenia
1. Krawędzie kubełków histogramów (propozycja wyżej) — OK czy inne progi?
2. Retencja/kompakcja `model_metrics_rollup` (np. surowe godzinowe 90 dni → dzienne agregaty)?
3. Eksport: CSV wystarczy, czy też PDF (jak mockup m06)?
4. Czy `service_id`/`backend`/`modality` są dostępne w `AiGateway` kontekście, czy trzeba
   je doprowadzić (do sprawdzenia w Chunku 2 — może wymagać przekazania z executora).
