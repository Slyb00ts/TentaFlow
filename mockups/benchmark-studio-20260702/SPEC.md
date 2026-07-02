# Benchmark Studio — spec współdzielony mockupów

Benchmarki wydajności modeli LLM przez API — jak `llama-bench`, ale dla całych
serwisów w mesh TentaFlow i zewnętrznych API. Wszystkie mockupy dzielą
`shared/styles.css` (skopiowany z model-metrics-20260630 + sekcja rozszerzeń
Benchmark Studio na końcu pliku).

## Zasady wizualne (NIE łamać)
- Tokeny i klasy jak w model-metrics: `.kpi`, `.tf-table`, `.section-card`,
  `.chip/.status-pill`, `.segmented`, `.btn`, `.grid-2/.grid-3`, `.mono`,
  `.breadcrumb`, `.app/.sidebar/.main`. Sidebar: „Benchmark Studio" aktywny
  w sekcji „Analityka".
- Wykresy: czysty inline SVG — ZERO bibliotek JS.
- Wyniki ZAWSZE jako mean ± σ z N powtórzeń (jak llama-bench), percentyle
  p50/p90/p99 dla latencji. Bez kosztów (zł) — to nie billing.
- Kolory serii (zmienne w styles.css): `--serie-gemma` #6366f1,
  `--serie-qwen` #a78bfa, `--serie-claude` #60a5fa.
- Polski w UI. Jednostki: ms, t/s, s, min, %.

## Flow użytkownika
1. **M1 Lista** → „+ Nowy benchmark"
2. **M2 Kreator 1/2** — nazwa + targety: (a) serwisy z mesh (checkbox: model,
   node, silnik, status online), (b) zewnętrzne API (typ OpenAI/Anthropic,
   host, port, opcjonalny API key, nazwa modelu)
3. **M3 Kreator 2/2** — testy z toggle + parametrami, estymacja czasu,
   „Uruchom benchmark"
4. **M4 Run live** — progress, macierz target×test, live log, wyniki częściowe
5. **M5 Wyniki** — tabela à la llama-bench + 4 wykresy, eksport CSV
6. **M6 Porównanie** — Run A vs Run B, delty Δ%, sekcja „Regresje"

## Testy (definicje)
| Test | Parametry | Co mierzy |
|------|-----------|-----------|
| Latencja pojedyncza | prompt 512, gen 128, n=5 | czyste TTFT / prefill / decode bez rywalizacji |
| Sweep równoległości | c = 1, 4, 16, 64; p512 g128 | throughput agregowany + spadek per-request |
| Sweep kontekstu | ctx = 128, 2048, 8192, 32768; g128, n=3 | degradacja TTFT/decode z długością promptu |
| Stabilność (sustained) | 10 min, c=8, p512 g128 | degradacja/hangi/throttling w długim ruchu |

## Metryki
TTFT ms (mean±σ), prefill t/s, decode t/s (mean±σ, per-request przy c>1),
total ms, latencje p50/p90/p99, throughput agregowany t/s przy N równoległych,
error rate %, stabilność w czasie (średnia krocząca 30 s decode t/s).
Źródło: streaming przez API targetu (timestamp pierwszego tokenu = TTFT,
tempo kolejnych tokenów = decode); prefill dla zewnętrznych API = „—"
(brak danych usage per-fazę).

## Dane demo (SPÓJNE między ekranami)
Targety (3):
- `gemma-4-31b` — spark-cluster (rig24+rig25, TP=2), vLLM 0.10.0, seria #6366f1
- `Qwen2.5-7B` — rig24, vLLM 0.10.0, seria #a78bfa
- `claude-sonnet` — Anthropic API (api.anthropic.com:443), seria #60a5fa

Wyniki Run #14 „LLM produkcyjne Q3" (02.07.2026 09:14, 34 min, success):

| Metryka | gemma-4-31b | Qwen2.5-7B | claude-sonnet |
|---|---|---|---|
| TTFT @p512 (ms) | 420±18 | 158±9 | 780±140 |
| Prefill (t/s) | 890 | 3 240 | — |
| Decode @c=1 (t/s) | 24.1±0.4 | 57.6±1.1 | 61.8±4.2 |
| p50/p90/p99 @c=1 (ms) | 5 720/5 840/5 920 | 2 380/2 440/2 510 | 2 850/3 220/3 610 |
| Decode per-req @c=1/4/16/64 | 24.1/21.8/14.2/5.9 | 57.6/52.3/38.4/17.2 | 61.8/60.2/57.1/48.9 |
| Throughput @c=64 (t/s) | 378 | 1 101 | 914 (2.1% err 429) |
| Decode @ctx 128/2k/8k/32k | 24.6/23.8/21.4/16.9 | 58.4/56.9/52.1/43.6 | 62.5/61.0/58.7/54.2 |
| TTFT @ctx 32k (ms) | 9 800±420 | 3 900±160 | 3 100±480 |
| Stabilność 10 min c=8 | 20.9±4.6 (dip do ~14 w min 6–7) | 44.9±1.3 | 59.6±3.1 |

Porównanie M6 (benchmark „gemma-4 tuning silnika"): Run A #9 (24.06, vLLM 0.9.2)
vs Run B #12 (01.07, vLLM 0.10.0). Poprawa: TTFT −8.3%, prefill +14.6%,
decode c=1 +9.1% (24.1→26.3). Regresje: decode @ctx 32k −17.8% (16.9→13.9),
TTFT @ctx 32k +16.3% (9 800→11 400).

Benchmarki na liście (M1): „LLM produkcyjne Q3" (3×4), „gemma-4 tuning silnika"
(1×2: latencja + kontekst), „Zewnętrzne API vs lokalne" (3×1, run #13 w toku),
„Nocny regres 7B" (1×4, #11 failed timeout). Runy: #14 success 34 min,
#13 running, #12 success 18 min, #11 failed 9 min, #10 success 33 min, #9 success 19 min.

## Protokół — szkic `BenchmarkBody`
Dashboard rozmawia WYŁĄCZNIE binarnym protokołem (`MessageBody::BenchmarkBody`
+ `tentaflow-protocol/src/benchmark.rs`), zero REST. Runner żyje w Core
(`tentaflow-core/src/benchmark/`), wyniki w SQLite (`benchmark_defs`,
`benchmark_runs`, `benchmark_results`).

```rust
enum BenchmarkBody {
  // definicje
  ListBenchmarks, BenchmarkList(Vec<BenchmarkSummary>),
  SaveBenchmark(BenchmarkDef),          // nazwa + targets + tests (upsert)
  DeleteBenchmark { benchmark_id: Uuid },
  // wykonanie
  StartRun { benchmark_id: Uuid }, RunStarted { run_id: Uuid },
  StopRun { run_id: Uuid },
  SubscribeRun { run_id: Uuid },        // push: progress, macierz, log, partial results
  RunEvent(RunEvent),                   // Progress | CellStatus | LogLine | PartialResult | Finished
  // wyniki i historia
  ListRuns { benchmark_id: Option<Uuid> }, RunList(Vec<RunSummary>),
  GetRunResults { run_id: Uuid }, RunResults(Vec<ResultRow>),
  CompareRuns { run_a: Uuid, run_b: Uuid }, RunComparison { deltas: Vec<MetricDelta>, regressions: Vec<MetricDelta> },
  ExportCsv { run_id: Uuid }, CsvData(String),
}

struct TargetDef {
  kind: TargetKind,                     // MeshService { service_id } | ExternalApi { api: OpenAi|Anthropic, host, port, api_key_enc: Option<..>, model }
}
struct TestDef { kind: TestKind, enabled: bool, params: TestParams }
// TestKind: SingleLatency | ConcurrencySweep | ContextSweep | SustainedLoad
struct ResultRow {
  target_id: Uuid, test: TestKind, variant: String,  // np. "c=16", "ctx=32768"
  ttft_ms: Stat, prefill_tps: Option<f64>, decode_tps: Stat,
  total_ms: Stat, p50_ms: f64, p90_ms: f64, p99_ms: f64,
  throughput_tps: Option<f64>, error_rate: f64, samples: u32,
}
struct Stat { mean: f64, sigma: f64 }   // mean ± σ z N powtórzeń
```

Regresja = pogorszenie metryki > 5% vs Run A (kierunek zależny od metryki:
TTFT/latencja niżej=lepiej, decode/prefill/throughput wyżej=lepiej).
API keys zewnętrznych targetów szyfrowane jak external secrets; listing
zwraca `<redacted>`.

## Strony
- `index.html` — opis zestawu + 3 karty zasad + grid linków M1–M6.
- `m01-lista.html` — karty benchmarków (targety, testy, ostatni run, sparkline
  trendu decode t/s) + „Ostatnie runy".
- `m02-kreator-targety.html` — krok 1/2: nazwa, serwisy mesh (checkbox),
  zewnętrzne API (formularz dodania), stepper 1-2.
- `m03-kreator-testy.html` — krok 2/2: 4 karty testów z toggle + parametry,
  podsumowanie „2 targety z mesh + 1 zewnętrzny × 4 testy ≈ 35 min", „Uruchom".
- `m04-run-live.html` — elapsed + Stop, progress 45%, macierz ✓/▶/⏳,
  live log monospace, wyniki częściowe.
- `m05-wyniki.html` — tabela à la llama-bench (grupy per test, ★ najlepszy
  w kolumnie) + 4 wykresy SVG: słupki decode per równoległość, linie degradacji
  vs kontekst, poziome słupki p50/p90/p99, timeline stabilności z dipem gemma-4.
- `m06-porownanie.html` — selektory Run A/B, chipy „Regresje", tabela delt Δ%
  (zielony/czerwony), słupki A-vs-B decode per wariant (regresja czerwona).
