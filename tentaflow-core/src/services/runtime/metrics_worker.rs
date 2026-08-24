// =============================================================================
// Plik: services/runtime/metrics_worker.rs — batchowy zapis model_metrics_rollup
// =============================================================================
// Zapis `model_metrics_rollup` byl jedynym synchronicznym DB-write na sciezce
// requestu /v1 — bral globalny pisarz SQLite (mutex) i blokowal watek tokio per
// zadanie. Ten worker przenosi zapis na JEDEN dedykowany watek OS i DODATKOWO
// akumuluje inkrementacje w pamieci: flush co ~200 ms LUB po 512 jobach
// (whichever first) wpisuje cala partie jako JEDNA transakcja pisarza. W tym
// samym COMMIT-cie leca tez delty `token_usage_daily` z `token_usage_cache`
// (read-your-writes enforcement czyta pamiec natychmiast, baza dostaje delty
// batchowo).
//
// W przeciwienstwie do `compliance::audit_worker` `submit_rollup_bump` NIGDY
// nie blokuje: kolejka jest ograniczona, a przy pelnej job jest odrzucony.
// Metryki to komutacyjne inkrementacje licznikow — kolejnosc i podzial na
// transakcje sa bez znaczenia dla wyniku, wiec utrata pojedynczego bumpu przy
// patologicznym przeciazeniu jest akceptowalna, a blokowanie watka requestu — nie.
//
// Gdy worker nie jest zainicjowany (testy / bootstrap bez DB) bump wykonuje sie
// inline na callerze — zachowanie zbiezne ze synchronicznym zapisem.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::OnceLock;
use std::time::Duration;

use crate::db::models::{
    ModelMetricsCounters, ModelMetricsDims, ModelMetricsPerfSamples, ModelMetricsTimes,
    ModelMetricsTokens,
};
use crate::db::DbPool;

/// Owned wymiary kubelka rollupu — wersja `ModelMetricsDims` na Stringach,
/// przenoszalna miedzy watkami (kanal + akumulator).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMetricsDimsOwned {
    pub node_id: String,
    pub org_id: String,
    pub user_id: String,
    pub model_id: String,
    pub service_key: String,
    pub backend: String,
    pub modality: String,
    pub hour_bucket: String,
    pub histogram_version: i64,
}

impl ModelMetricsDimsOwned {
    fn borrowed(&self) -> ModelMetricsDims<'_> {
        ModelMetricsDims {
            node_id: &self.node_id,
            org_id: &self.org_id,
            user_id: &self.user_id,
            model_id: &self.model_id,
            service_key: &self.service_key,
            backend: &self.backend,
            modality: &self.modality,
            hour_bucket: &self.hour_bucket,
            histogram_version: self.histogram_version,
        }
    }
}

/// Jeden bump metryk zbudowany na sciezce requestu: owned wymiary + skopiowane
/// liczniki. `db` sluzy TYLKO sciezce inline (worker nie zainicjowany / tryb
/// sync / martwy kanal) — batchowy flush pisze do puli przekazanej przy
/// `init_metrics_worker`, wiec wszystkie joby jednego workera musza dzielic
/// jedna pule (w produkcji zawsze prawda; kazdy test spawnuje wlasny worker).
#[derive(Debug)]
pub struct RollupBump {
    pub db: DbPool,
    pub dims: ModelMetricsDimsOwned,
    pub counters: ModelMetricsCounters,
    pub tokens: ModelMetricsTokens,
    pub times: ModelMetricsTimes,
    pub perf: ModelMetricsPerfSamples,
}

/// Zsumowana delta jednego kubelka (klucz = `model_metrics_id` z wymiarow).
/// Kubełki histogramow licza PROBKI (ile pomiarow wpadlo do kubelka), wiec merge
/// to zwykly add element-wise.
#[derive(Debug, Clone, PartialEq)]
struct RollupDelta {
    dims: ModelMetricsDimsOwned,
    counters: ModelMetricsCounters,
    tokens: ModelMetricsTokens,
    times: ModelMetricsTimes,
    ttft: [i64; crate::db::repository::TTFT_BUCKETS],
    decode_tps: [i64; crate::db::repository::DECODE_TPS_BUCKETS],
    e2e: [i64; crate::db::repository::E2E_BUCKETS],
}

impl RollupDelta {
    fn from_bump(bump: &RollupBump) -> Self {
        let mut delta = Self {
            dims: bump.dims.clone(),
            counters: bump.counters,
            tokens: bump.tokens,
            times: bump.times,
            ttft: [0; crate::db::repository::TTFT_BUCKETS],
            decode_tps: [0; crate::db::repository::DECODE_TPS_BUCKETS],
            e2e: [0; crate::db::repository::E2E_BUCKETS],
        };
        if let Some(v) = bump.perf.ttft_ms {
            delta.ttft[crate::db::repository::histogram_bucket_index(
                &crate::db::repository::TTFT_MS_EDGES,
                v,
            )] = 1;
        }
        if let Some(v) = bump.perf.decode_tps {
            delta.decode_tps[crate::db::repository::histogram_bucket_index(
                &crate::db::repository::DECODE_TPS_EDGES,
                v,
            )] = 1;
        }
        if let Some(v) = bump.perf.e2e_ms {
            delta.e2e[crate::db::repository::histogram_bucket_index(
                &crate::db::repository::E2E_MS_EDGES,
                v,
            )] = 1;
        }
        delta
    }

    fn merge_bump(&mut self, bump: &RollupBump) {
        self.counters.request_count += bump.counters.request_count;
        self.counters.success_count += bump.counters.success_count;
        self.counters.error_count += bump.counters.error_count;
        self.counters.usage_missing_count += bump.counters.usage_missing_count;
        self.tokens.prompt_tokens += bump.tokens.prompt_tokens;
        self.tokens.completion_tokens += bump.tokens.completion_tokens;
        self.tokens.total_tokens += bump.tokens.total_tokens;
        self.tokens.embedding_tokens += bump.tokens.embedding_tokens;
        self.tokens.audio_ms += bump.tokens.audio_ms;
        self.tokens.images += bump.tokens.images;
        self.times.prefill_secs += bump.times.prefill_secs;
        self.times.decode_secs += bump.times.decode_secs;
        self.times.e2e_latency_ms += bump.times.e2e_latency_ms;
        self.times.queue_ms += bump.times.queue_ms;
        if let Some(v) = bump.perf.ttft_ms {
            self.ttft[crate::db::repository::histogram_bucket_index(
                &crate::db::repository::TTFT_MS_EDGES,
                v,
            )] += 1;
        }
        if let Some(v) = bump.perf.decode_tps {
            self.decode_tps[crate::db::repository::histogram_bucket_index(
                &crate::db::repository::DECODE_TPS_EDGES,
                v,
            )] += 1;
        }
        if let Some(v) = bump.perf.e2e_ms {
            self.e2e[crate::db::repository::histogram_bucket_index(
                &crate::db::repository::E2E_MS_EDGES,
                v,
            )] += 1;
        }
    }
}

/// Async jest domyslny.
static ASYNC_ENABLED: AtomicBool = AtomicBool::new(true);

/// Ograniczony kanal do watka pracujacego. `None` dopoki `init_metrics_worker`
/// nie zalezci; callerzy wtedy wykonuja bump inline (testy / bootstrap bez DB).
static SENDER: OnceLock<SyncSender<RollupBump>> = OnceLock::new();

/// Bump metryki to kilka owned Stringow (kilkaset bajtow), wiec 8192 stanow to
/// kilka MB pamieci przy calkowitym przeciazeniu. Jeden worker wytrzymuje wiecej
/// niz realne tempo requestow, granica jest tylko na wypadek awaryjnego zatoru.
const QUEUE_CAPACITY: usize = 8192;

/// Okno flusha: tyle worker czeka na kolejny bump, zanim wpisze akumulowane
/// delty do bazy.
const FLUSH_WINDOW: Duration = Duration::from_millis(200);

/// Prog jobow wymuszajacy flush przed uplywem okna (ochrona przy skoku ruchu —
/// akumulator nie rosnie nieograniczonie miedzy timerami).
const FLUSH_MAX_JOBS: usize = 512;

/// Maksymalna liczba statementow w jednej transakcji flusha — wieksza partie
/// dzielimy na kilka COMMIT-ow, zeby nie trzymac pisarza bezterminowo.
const MAX_STATEMENTS_PER_TX: usize = 1000;

/// Po tej liczbie KOLEJNYCH nieudanych flushow ta sama partia jest zrzucana —
/// bez tego trwale uszkodzona baza zatrzymywalaby akumulator na zawsze.
const MAX_CONSECUTIVE_FLUSH_FAILURES: u32 = 3;

/// Tryb zapisu metryk. `true` = async (domyslnie), `false` = synchroniczny
/// inline (dla testow).
pub fn set_metrics_async(enabled: bool) {
    ASYNC_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Czy zapis metryk jest odwroczony do watka tla.
pub fn metrics_async_enabled() -> bool {
    ASYNC_ENABLED.load(Ordering::Relaxed)
}

/// Stan akumulatora jednego workera. `Mutex` nie jest potrzebny do pracy
/// (akumulator zyje tylko na watku workera), ale trzymamy stan w osobnej
/// strukturze, zeby testy mogly drenowac go recznie bez watkow.
#[derive(Default)]
struct Accum {
    deltas: HashMap<String, RollupDelta>,
    pending_jobs: usize,
    consecutive_flush_failures: u32,
}

impl Accum {
    fn push(&mut self, bump: RollupBump) {
        let id = crate::db::repository::model_metrics_id(&bump.dims.borrowed());
        match self.deltas.get_mut(&id) {
            Some(delta) => delta.merge_bump(&bump),
            None => {
                self.deltas.insert(id, RollupDelta::from_bump(&bump));
            }
        }
        self.pending_jobs += 1;
    }

    /// Drenuje akumulowane delty (i zeruje licznik jobow do progu flusha).
    fn take_deltas(&mut self) -> Vec<(String, RollupDelta)> {
        self.pending_jobs = 0;
        self.deltas.drain().collect()
    }

    /// Zwraca partie do akumulatora po nieudanym flushu (retencja do retry).
    fn restore(&mut self, deltas: Vec<(String, RollupDelta)>) {
        for (id, delta) in deltas {
            match self.deltas.get_mut(&id) {
                Some(existing) => existing.merge_delta(&delta),
                None => {
                    self.deltas.insert(id, delta);
                }
            }
        }
    }
}

impl RollupDelta {
    /// Merge dwóch delt (add element-wise) — używany przy retencji, gdy część
    /// partii wraca do akumulatora, w którym zdążył już urosnąć nowy bump.
    fn merge_delta(&mut self, other: &RollupDelta) {
        self.counters.request_count += other.counters.request_count;
        self.counters.success_count += other.counters.success_count;
        self.counters.error_count += other.counters.error_count;
        self.counters.usage_missing_count += other.counters.usage_missing_count;
        self.tokens.prompt_tokens += other.tokens.prompt_tokens;
        self.tokens.completion_tokens += other.tokens.completion_tokens;
        self.tokens.total_tokens += other.tokens.total_tokens;
        self.tokens.embedding_tokens += other.tokens.embedding_tokens;
        self.tokens.audio_ms += other.tokens.audio_ms;
        self.tokens.images += other.tokens.images;
        self.times.prefill_secs += other.times.prefill_secs;
        self.times.decode_secs += other.times.decode_secs;
        self.times.e2e_latency_ms += other.times.e2e_latency_ms;
        self.times.queue_ms += other.times.queue_ms;
        for (dst, src) in self.ttft.iter_mut().zip(&other.ttft) {
            *dst += src;
        }
        for (dst, src) in self.decode_tps.iter_mut().zip(&other.decode_tps) {
            *dst += src;
        }
        for (dst, src) in self.e2e.iter_mut().zip(&other.e2e) {
            *dst += src;
        }
    }
}

/// Jeden wpis partii flusha — rollupy i delty zuzycia w jednej sekwencji, wiec
/// chunking po `MAX_STATEMENTS_PER_TX` obejmuje oba rodzaje zapisow.
enum BatchWrite {
    Rollup(String, RollupDelta),
    Usage(
        crate::services::runtime::token_usage_cache::UsageKey,
        crate::services::runtime::token_usage_cache::UsageTotals,
    ),
}

/// Jeden cykl flusha: drenaz akumulatora + pobranie delt zuzycia + zapis calosci
/// transakcjami po `MAX_STATEMENTS_PER_TX` statementow. Rollupy i delty zuzycia
/// leca WSPOLNYM cyklem (pierwsze chunki mieszaja oba rodzaje), dzieki czemu
/// enforcement widzi spojny obraz po jednym COMMIT-cie. Zwraca liczbe zapisanych
/// statementow albo `Err(())` — wtedy rollupy wrociły do akumulatora, a delty
/// zuzycia z NIEudanych chunków wrociły do cache'a (retencja); watermark delty
/// jest przesuwany przy take, wiec ta sama delta nie moze byc zapisana dwukrotnie.
/// Cache przekazywany jawnie: produkcja uzywa instancji globalnej, testy wlasnej.
fn flush_batch(
    pool: &DbPool,
    acc: &mut Accum,
    cache: &crate::services::runtime::token_usage_cache::TokenUsageCache,
) -> Result<usize, ()> {
    use crate::services::runtime::token_usage_cache::UsageKey;
    let rollups = acc.take_deltas();
    let usage = cache.take_usage_delta();
    if rollups.is_empty() && usage.is_empty() {
        return Ok(0);
    }
    let mut writes: Vec<BatchWrite> = rollups
        .into_iter()
        .map(|(id, delta)| BatchWrite::Rollup(id, delta))
        .collect();
    writes.extend(
        usage
            .iter()
            .map(|(key, totals)| BatchWrite::Usage(key.clone(), *totals)),
    );

    let mut committed_keys: Vec<UsageKey> = Vec::new();
    let mut retained_rollups: Vec<(String, RollupDelta)> = Vec::new();
    let mut first_failed_chunk: Option<usize> = None;
    for (chunk_idx, chunk) in writes.chunks(MAX_STATEMENTS_PER_TX).enumerate() {
        let result = crate::db::repository::with_writer_tx(pool, |tx| {
            for write in chunk {
                match write {
                    BatchWrite::Rollup(id, delta) => {
                        let dims = delta.dims.borrowed();
                        crate::db::repository::upsert_model_metrics_row_on_tx(
                            tx,
                            id,
                            &dims,
                            &delta.counters,
                            &delta.tokens,
                            &delta.times,
                            &delta.ttft,
                            &delta.decode_tps,
                            &delta.e2e,
                        )?;
                    }
                    BatchWrite::Usage(key, totals) => {
                        crate::db::repository::bump_token_usage_delta_on_tx(
                            tx,
                            &crate::db::repository::TokenUsageDelta {
                                node_id: &key.node_id,
                                org_id: &key.org_id,
                                user_id: &key.user_id,
                                model_id: &key.model_id,
                                usage_day: &key.day,
                                prompt_tokens: totals.prompt_tokens,
                                completion_tokens: totals.completion_tokens,
                                embedding_tokens: totals.embedding_tokens,
                                audio_ms: totals.audio_ms,
                                request_count: totals.request_count,
                            },
                        )?;
                    }
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => {
                for write in chunk {
                    if let BatchWrite::Usage(key, _) = write {
                        committed_keys.push(key.clone());
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    chunk = chunk_idx,
                    error = %e,
                    "metrics-worker: flush partii nieudany — retencja do nastepnego cyklu"
                );
                first_failed_chunk = Some(chunk_idx);
                break;
            }
        }
    }

    // Delty z udanych chunków sa juz trwale zapisane — ewikcja niezaleznie od
    // losów dalszych chunków, zeby wpisy o zerowej delcie nie wisialy w mapie.
    if !committed_keys.is_empty() {
        cache.evict_persisted(committed_keys.iter());
    }
    let mut failed_usage: Vec<(
        UsageKey,
        crate::services::runtime::token_usage_cache::UsageTotals,
    )> = Vec::new();
    if let Some(failed_idx) = first_failed_chunk {
        for write in &writes[failed_idx * MAX_STATEMENTS_PER_TX..] {
            match write {
                BatchWrite::Rollup(id, delta) => retained_rollups.push((id.clone(), delta.clone())),
                BatchWrite::Usage(key, totals) => failed_usage.push((key.clone(), *totals)),
            }
        }
        acc.consecutive_flush_failures += 1;
        if acc.consecutive_flush_failures >= MAX_CONSECUTIVE_FLUSH_FAILURES {
            tracing::error!(
                dropped_rollups = retained_rollups.len(),
                failures = acc.consecutive_flush_failures,
                "metrics-worker: partia rollupow zrzucona po kolejnych nieudanych flushach"
            );
            acc.consecutive_flush_failures = 0;
        } else {
            acc.restore(retained_rollups);
        }
        // Delty zuzycia NIGDY nie sa zrzucane — retencja bez limitu do skutku.
        if !failed_usage.is_empty() {
            cache.restore_usage_delta(&failed_usage);
        }
        return Err(());
    }

    acc.consecutive_flush_failures = 0;
    Ok(writes.len())
}

/// Wynik jednej proby flusha — obserwowalnosc dla testow (watki nie moga byc
/// joinowane deterministycznie, wiec worker raportuje przez kanal).
#[derive(Debug, PartialEq)]
enum FlushOutcome {
    Wrote(usize),
    Failed,
}

type FlushHook = Box<dyn FnMut(FlushOutcome) + Send>;

/// Petla workera: recv z oknem 200 ms; prog jobow wymusza flush przed timerem.
/// Kazda iteracja jest izolowana catch_unwind — panic w jednym cyklu nie moze
/// zabijac workera (ucichlyby wszystkie pozniejsze metryki).
fn run_worker_loop(
    rx: Receiver<RollupBump>,
    pool: DbPool,
    cache: std::sync::Arc<crate::services::runtime::token_usage_cache::TokenUsageCache>,
    window: Duration,
    max_jobs: usize,
    mut on_flush: Option<FlushHook>,
) {
    let mut acc = Accum::default();
    loop {
        let received = rx.recv_timeout(window);
        // Flaga przed move do catch_unwind — po niej `received` juz nie zyje.
        let disconnected = matches!(
            received,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        );
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match received {
            Ok(bump) => {
                acc.push(bump);
                if acc.pending_jobs >= max_jobs {
                    flush_batch(&pool, &mut acc, &cache)
                } else {
                    Ok(0)
                }
            }
            // Timeout = deadline okna; Disconnected = koniec zycia kanalu —
            // ostatni flush przed wyjsciem.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                flush_batch(&pool, &mut acc, &cache)
            }
        }));
        match outcome {
            Ok(Ok(0)) => {}
            Ok(Ok(written)) => {
                if let Some(hook) = on_flush.as_mut() {
                    hook(FlushOutcome::Wrote(written));
                }
            }
            Ok(Err(())) => {
                if let Some(hook) = on_flush.as_mut() {
                    hook(FlushOutcome::Failed);
                }
            }
            Err(panic) => {
                tracing::error!(
                    ?panic,
                    "metrics-worker: cykl pracy panikowal (partia utracona)"
                );
            }
        }
        if disconnected {
            tracing::info!("metrics-worker: kanal zamkniety, worker konczy prace");
            return;
        }
    }
}

/// Buduje lokalny kanal + watek drenujacy. Wydzielone z `init_metrics_worker`,
/// zeby testy mogly sprawdzac zachowanie workera (progi, okno, retencja) bez
/// dotykania globalnego `SENDER` — w przeciwnym razie rownolegle testy rollupow
/// w tym samym procesie przestalyby byc deterministyczne.
fn spawn_worker(
    pool: DbPool,
    cache: std::sync::Arc<crate::services::runtime::token_usage_cache::TokenUsageCache>,
    window: Duration,
    max_jobs: usize,
    on_flush: Option<FlushHook>,
) -> SyncSender<RollupBump> {
    let (tx, rx) = sync_channel::<RollupBump>(QUEUE_CAPACITY);
    std::thread::Builder::new()
        .name("metrics-worker".to_string())
        .spawn(move || run_worker_loop(rx, pool, cache, window, max_jobs, on_flush))
        .expect("spawn metrics-worker thread");
    tx
}

/// Spawnuje pojedynczy watek pracujacy. Idempotentne — druga wywolka to no-op.
/// Bezpieczne do wywolania bezwarunkowo po inicjalizacji DB; w trybie sync worker
/// po prostu nigdy nie dostanie jobow.
pub fn init_metrics_worker(pool: DbPool) {
    if SENDER.get().is_some() {
        return;
    }
    let tx = spawn_worker(
        pool,
        crate::services::runtime::token_usage_cache::global().clone(),
        FLUSH_WINDOW,
        FLUSH_MAX_JOBS,
        None,
    );
    // Ignorujemy race, gdy dwoch callerzy inituje naraz — przegrany gubi swoj
    // tx (z nim receiver), a watek zwyciezcy jest tym zywym.
    if SENDER.set(tx).is_err() {
        return;
    }
}

/// Zapisuje jeden bump metryk. W trybie async bump trafia do watka tla przez
/// `try_send`: przy pelnej kolejce jest odrzucony (warn), przy umarlym workerze
/// (zamkniety kanal) — wykonywany inline jako ostatnia desperacja. W trybie sync
/// albo gdy worker nie jest zainicjowany, bump idzie inline.
pub fn submit_rollup_bump(bump: RollupBump) {
    if metrics_async_enabled() {
        if let Some(tx) = SENDER.get() {
            match tx.try_send(bump) {
                Ok(()) => return,
                // Drop zamiast blokady: hot-path nie moze czekac, a kolejnosc
                // bumpow nie ma znaczenia (komutacyjne inkrementacje). Warn per
                // odrzucony bump, zeby przeciazenie bylo widoczne.
                Err(TrySendError::Full(_)) => {
                    tracing::warn!("metrics-worker: kolejka pelna, zrzucam bump metryk");
                    return;
                }
                // Worker juz nie zyje — kanal zwraca bump, wiec wykonujemy go inline.
                Err(TrySendError::Disconnected(bump)) => {
                    tracing::warn!("metrics-worker: kanal martwy, bump inline");
                    apply_bump_inline(bump);
                    return;
                }
            }
        }
    }
    apply_bump_inline(bump);
}

/// Sciezka inline: jeden bump = jedna transakcja przez `bump_model_metrics_rollup`.
/// Uzywana w testach, przy bootstrapie bez workera i jako fallback za martwym
/// kanalem — zachowanie zbiezne z poprzednim synchronicznym zapisem.
fn apply_bump_inline(bump: RollupBump) {
    let dims = bump.dims.borrowed();
    if let Err(e) = crate::db::repository::bump_model_metrics_rollup(
        &bump.db,
        &dims,
        &bump.counters,
        &bump.tokens,
        &bump.times,
        &bump.perf,
    ) {
        tracing::warn!(
            model_id = %bump.dims.model_id,
            error = %e,
            "model metrics rollup bump failed (metrics dropped, request unaffected)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn pool() -> DbPool {
        crate::db::init(std::path::Path::new(":memory:")).expect("init test db")
    }

    fn read_only_pool(path: &std::path::Path) -> DbPool {
        crate::db::init_read_only(path).expect("init read-only db")
    }

    fn bump(db: DbPool, node: &str, model: &str, prompt: i64, completion: i64) -> RollupBump {
        RollupBump {
            db,
            dims: ModelMetricsDimsOwned {
                node_id: node.to_string(),
                org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
                user_id: "u1".to_string(),
                model_id: model.to_string(),
                service_key: format!("http/{model}"),
                backend: "http".to_string(),
                modality: "chat".to_string(),
                hour_bucket: "2026-08-23T10:00:00Z".to_string(),
                histogram_version: crate::db::repository::MODEL_METRICS_HISTOGRAM_VERSION,
            },
            counters: ModelMetricsCounters {
                request_count: 1,
                success_count: 1,
                error_count: 0,
                usage_missing_count: 0,
            },
            tokens: ModelMetricsTokens {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: prompt + completion,
                embedding_tokens: 0,
                audio_ms: 0,
                images: 0,
            },
            times: ModelMetricsTimes::default(),
            perf: ModelMetricsPerfSamples::default(),
        }
    }

    fn rows(db: &DbPool) -> Vec<crate::db::models::DbModelMetricsRollup> {
        crate::db::repository::list_model_metrics_rollup(
            db,
            crate::services::org::DEFAULT_ORG_ID,
            &Default::default(),
        )
        .expect("list rollup")
    }

    /// Lokalny cache bez DB — testy petli workera nie zapisuja zadnego zuzycia,
    /// wiec nie dotykaja globalnej instancji (izolacja miedzy testami).
    fn no_db_cache() -> std::sync::Arc<crate::services::runtime::token_usage_cache::TokenUsageCache>
    {
        std::sync::Arc::new(crate::services::runtime::token_usage_cache::TokenUsageCache::new(None))
    }

    /// Gdy worker nie jest zainicjowany (standard w testach — `SENDER` jest
    /// `None`) bump MUSI dzialac inline na callerze: testy rollupow czytaja baze
    /// od razu po submit i nie moga czekac na watek.
    #[test]
    fn submit_runs_inline_when_worker_not_wired() {
        let db = pool();
        submit_rollup_bump(bump(db.clone(), "node-A", "qwen-chat", 10, 5));
        let all = rows(&db);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].request_count, 1);
        assert_eq!(all[0].total_tokens, 15);
    }

    /// Tryb sync wymusza inline — nawet przy zainicjowanym workerze bump nie
    /// moze poleciec do kolejki.
    #[test]
    fn sync_mode_forces_inline_execution() {
        let db = pool();
        let prev = metrics_async_enabled();
        set_metrics_async(false);
        submit_rollup_bump(bump(db.clone(), "node-A", "qwen-chat", 7, 3));
        set_metrics_async(prev);
        let all = rows(&db);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].total_tokens, 10);
    }

    /// Worker wykonuje bump na swoim watku (nie na callerze): caller nie widzi
    /// wiersza od razu, dopiero hook flusha potwierdza zapis.
    #[test]
    fn worker_runs_bumps_on_its_own_thread() {
        let db = pool();
        let (notify_tx, notify_rx) = mpsc::channel();
        let tx = spawn_worker(
            db.clone(),
            no_db_cache(),
            Duration::from_secs(3600),
            1,
            Some(Box::new(move |outcome| {
                let _ = notify_tx.send(outcome);
            })),
        );
        tx.send(bump(db.clone(), "node-A", "qwen-chat", 4, 1))
            .expect("queue bump");
        assert_eq!(
            notify_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("flush"),
            FlushOutcome::Wrote(1)
        );
        assert_eq!(rows(&db)[0].total_tokens, 5);
    }

    /// Nieudany flush (baza tylko do odczytu) nie zabija petli workera: kolejne
    /// bumpi sa dalej przyjmowane i probowane, a worker konczy grzecznie po
    /// zamknieciu kanalu.
    #[test]
    fn failed_flush_does_not_kill_the_worker_loop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("ro.db");
        // Pisarz tworzy schemat, potem read-only pool z failujacym zapisem.
        drop(pool_at(&db_path));
        let db = read_only_pool(&db_path);
        let (notify_tx, notify_rx) = mpsc::channel();
        let tx = spawn_worker(
            db.clone(),
            no_db_cache(),
            Duration::from_secs(3600),
            1,
            Some(Box::new(move |outcome| {
                let _ = notify_tx.send(outcome);
            })),
        );
        for _ in 0..3 {
            tx.send(bump(db.clone(), "node-A", "qwen-chat", 1, 0))
                .expect("queue bump");
            assert_eq!(
                notify_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("flush"),
                FlushOutcome::Failed
            );
        }
        drop(tx);
    }

    fn pool_at(path: &std::path::Path) -> DbPool {
        crate::db::init(path).expect("init db at path")
    }

    /// Ograniczony kanal odrzuca (`Full`) zamiast blokowac nadawce — hot-path
    /// nigdy nie czeka na workerze.
    #[test]
    fn full_queue_drops_bump_instead_of_blocking() {
        let (tx, rx) = sync_channel::<RollupBump>(2);
        // Receiver zyje, ale nie odbiera — bufor sie zapelnia i try_send pada.
        tx.send(bump(pool(), "n", "m", 0, 0)).expect("send 1");
        tx.send(bump(pool(), "n", "m", 0, 0)).expect("send 2");
        match tx.try_send(bump(pool(), "n", "m", 0, 0)) {
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) | Ok(()) => {
                panic!("oczekiwano Full przy pelnej kolejce")
            }
        }
        drop(rx);
    }

    /// Kilka bumpow tego samego kubelka + inny kubelk w JEDNEJ partii: flush
    /// pisze wszystko jednym cyklem, a przed flushem baza jest pusta (zapisy
    /// nie sa synchroniczne).
    #[test]
    fn batched_bumps_land_in_one_flush_transaction() {
        let db = pool();
        let mut acc = Accum::default();
        acc.push(bump(db.clone(), "node-A", "qwen-chat", 10, 5));
        acc.push(bump(db.clone(), "node-A", "qwen-chat", 20, 10));
        acc.push(bump(db.clone(), "node-A", "qwen-chat", 1, 1));
        acc.push(bump(db.clone(), "node-A", "bge-embed", 8, 0));
        assert!(
            rows(&db).is_empty(),
            "przed flushem nic nie moze byc zapisane"
        );

        let written = flush_batch(&db, &mut acc, &no_db_cache()).expect("flush ok");
        // Trzy bumpy tego samego kubelka mergeuja sie do JEDNEGO statementu —
        // piszemy 2 upserty (chat + embedding), nie 4.
        assert_eq!(written, 2);
        let all = rows(&db);
        assert_eq!(all.len(), 2);
        let chat = all
            .iter()
            .find(|r| r.model_id == "qwen-chat")
            .expect("chat row");
        assert_eq!(chat.request_count, 3);
        assert_eq!(chat.prompt_tokens, 31);
        assert_eq!(chat.completion_tokens, 16);
        assert_eq!(chat.total_tokens, 47);
        let emb = all
            .iter()
            .find(|r| r.model_id == "bge-embed")
            .expect("emb row");
        assert_eq!(emb.total_tokens, 8);
        assert!(acc.deltas.is_empty(), "akumulator po flushu pusty");
    }

    /// Prog jobow wymusza flush NATYCHMIAST, zanim uplynie okno timera
    /// (okno 3600 s nigdy nie zapala sie w tym tescie).
    #[test]
    fn threshold_triggers_flush_before_timer() {
        let db = pool();
        let (notify_tx, notify_rx) = mpsc::channel();
        let tx = spawn_worker(
            db.clone(),
            no_db_cache(),
            Duration::from_secs(3600),
            4,
            Some(Box::new(move |outcome| {
                let _ = notify_tx.send(outcome);
            })),
        );
        for i in 0..4 {
            tx.send(bump(db.clone(), "node-A", "qwen-chat", i, 0))
                .expect("queue bump");
        }
        assert_eq!(
            notify_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("flush"),
            FlushOutcome::Wrote(1),
            "4 bumpi tego samego kubelka = 1 zmergowany statement"
        );
        assert_eq!(rows(&db)[0].request_count, 4);
    }

    /// Maly ruch (< prog) leci po uplynieciu okna timera.
    #[test]
    fn timer_flushes_small_traffic() {
        let db = pool();
        let (notify_tx, notify_rx) = mpsc::channel();
        let tx = spawn_worker(
            db.clone(),
            no_db_cache(),
            Duration::from_millis(25),
            10_000,
            Some(Box::new(move |outcome| {
                let _ = notify_tx.send(outcome);
            })),
        );
        tx.send(bump(db.clone(), "node-A", "qwen-chat", 2, 2))
            .expect("queue bump");
        assert_eq!(
            notify_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("flush"),
            FlushOutcome::Wrote(1)
        );
        assert_eq!(rows(&db)[0].total_tokens, 4);
    }

    /// Merge delt jest komutacyjny: kolejność akumulacji bumpów o różnych
    /// wymiarach nie zmienia wynikowych sum ani kubełków.
    #[test]
    fn merge_is_commutative_across_dimensions() {
        let db = pool();
        let mut forward = Accum::default();
        forward.push(bump(db.clone(), "node-A", "qwen-chat", 10, 5));
        forward.push(bump(db.clone(), "node-A", "qwen-chat", 1, 1));
        forward.push(bump(db.clone(), "node-B", "bge-embed", 8, 0));
        forward.push(bump(db.clone(), "node-B", "bge-embed", 2, 0));

        let mut backward = Accum::default();
        backward.push(bump(db.clone(), "node-B", "bge-embed", 2, 0));
        backward.push(bump(db.clone(), "node-B", "bge-embed", 8, 0));
        backward.push(bump(db.clone(), "node-A", "qwen-chat", 1, 1));
        backward.push(bump(db.clone(), "node-A", "qwen-chat", 10, 5));

        let mut fw: Vec<_> = forward.take_deltas();
        let mut bw: Vec<_> = backward.take_deltas();
        fw.sort_by(|a, b| a.0.cmp(&b.0));
        bw.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(fw.len(), 2);
        assert_eq!(fw, bw, "merge musi byc komutacyjny");
    }

    /// Trzy kolejne nieudane flushy tej samej partii → partia zrzucona; przed
    /// tym retencja trzyma delty do retry.
    #[test]
    fn failed_flush_retries_then_drops_after_three_failures() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("ro.db");
        drop(pool_at(&db_path));
        let db = read_only_pool(&db_path);
        let mut acc = Accum::default();
        acc.push(bump(db.clone(), "node-A", "qwen-chat", 5, 5));

        assert!(flush_batch(&db, &mut acc, &no_db_cache()).is_err());
        assert_eq!(acc.deltas.len(), 1, "po 1. porazce partia zretencjonowana");
        assert!(flush_batch(&db, &mut acc, &no_db_cache()).is_err());
        assert_eq!(
            acc.deltas.len(),
            1,
            "po 2. porazce partia wciaz zretencjonowana"
        );
        assert!(flush_batch(&db, &mut acc, &no_db_cache()).is_err());
        assert!(acc.deltas.is_empty(), "po 3. porazce partia zrzucona");
        // Licznik porazek zresetowany — nowy bump startuje od nowa.
        acc.push(bump(db.clone(), "node-A", "qwen-chat", 1, 0));
        assert!(flush_batch(&db, &mut acc, &no_db_cache()).is_err());
        assert_eq!(acc.deltas.len(), 1);
    }

    /// Kubełki histogramow akumulują się tylko z ZMIERZONYCH probek: `None`
    /// nie dotyka histogramu (suma 0 = brak probek), a sample_count rośnie 1:1
    /// z liczbą probek.
    #[test]
    fn histogram_buckets_accumulate_only_measured_samples() {
        let db = pool();
        let mut acc = Accum::default();
        // ttft 30 ms → kubek 1 ([0,50) ); decode_tps/e2e bez pomiaru.
        let mut first = bump(db.clone(), "node-A", "qwen-chat", 0, 0);
        first.perf.ttft_ms = Some(30);
        acc.push(first);
        // ttft 700 ms → kubek 5; e2e 300 ms → kubek 2 ([250,500)).
        let mut second = bump(db.clone(), "node-A", "qwen-chat", 0, 0);
        second.perf.ttft_ms = Some(700);
        second.perf.e2e_ms = Some(300);
        acc.push(second);
        // Brak jakichkolwiek pomiarów.
        acc.push(bump(db.clone(), "node-A", "qwen-chat", 0, 0));

        flush_batch(&db, &mut acc, &no_db_cache()).expect("flush ok");
        let row = &rows(&db)[0];
        assert_eq!(row.ttft_buckets, [0, 1, 0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(row.ttft_sample_count, 2);
        assert_eq!(row.decode_tps_buckets, [0; 8]);
        assert_eq!(row.decode_tps_sample_count, 0);
        assert_eq!(row.e2e_buckets, [0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(row.e2e_sample_count, 1);
    }

    /// Flush wspólnym cyklem przenosi też delty `token_usage_daily`: zapis
    /// przez cache bez flusha nie dotyka bazy, po flushu delta ląduje w wierszu
    /// i watermark czyni ją niewidoczną dla kolejnego draina.
    #[test]
    fn shared_flush_cycle_writes_usage_deltas_too() {
        let db = pool();
        let org = crate::services::org::DEFAULT_ORG_ID;
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let cache = std::sync::Arc::new(
            crate::services::runtime::token_usage_cache::TokenUsageCache::new(Some(db.clone())),
        );
        let mut acc = Accum::default();
        acc.push(bump(db.clone(), "node-A", "qwen-chat", 3, 3));
        cache.record_chat_usage_on("node-A", org, "u1", "m1", &today, 100, 50);
        assert!(
            crate::db::repository::usage_summary(&db, org, "daily", &today, "user")
                .expect("summary")
                .is_empty(),
            "przed flushem token_usage_daily jest puste"
        );

        // Wspolny cykl: rollup + delta zuzycia w jednym wywolaniu flusha.
        let written = flush_batch(&db, &mut acc, &cache).expect("flush ok");
        assert_eq!(written, 2);
        let summary = crate::db::repository::usage_summary(&db, org, "daily", &today, "user")
            .expect("summary");
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].prompt_tokens, 100);
        assert_eq!(summary[0].completion_tokens, 50);
        assert_eq!(summary[0].total_tokens, 150);
        assert_eq!(summary[0].request_count, 1);
        // Watermark po udanym flushu = snapshot → drugi drain jest pusty.
        assert!(cache.pending_deltas_snapshot().is_empty());
    }
}
