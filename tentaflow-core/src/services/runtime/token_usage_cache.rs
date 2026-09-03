// =============================================================================
// Plik: services/runtime/token_usage_cache.rs — in-memory overlay token_usage_daily
// =============================================================================
// Zapisy `token_usage_daily` (chat / embeddings / STT) byly synchronicznymi
// UPSERT-ami na sciezce requestu — kazdy bral pisarza SQLite. Od teraz caller
// zapisuje inkrement DO PAMIECI tego cache'a, a metrics-worker flushuje delty
// batchowo we wlasnym cyklu. Dzieki temu:
//
// 1. Enforcement (koordynator dzierzaw) czyta pamiec NATYCHMIAST po zapisie
//    (read-your-writes) przez `overlay_usage_for_quota` — bez czekania na flush.
// 2. Baza dostaje jedna transakcje na cala partie zamiast jednej per request.
//
// Model danych: `totals` = stan klucza widziany z procesu (baseline wczytany
// leniwie z bazy przy pierwszym dotknieciu klucza + wszystkie inkrementy),
// `watermark` = ile z tego stanu zostalo juz zapisane do bazy. Delta flusha i
// delta dla enforcementu to to samo wyrazenie: `totals - watermark`. Po udanym
// flushu watermark jest przesuwany o zapisana partie, a wpisy o zerowej delcie
// sa usuwane (lazy eviction — stare dni znikaja same po ostatnim flushu).
//
// Rollover dobowy nie wymaga specjalnego kodu: nowy dzien to nowe klucze z
// wlasnym baseline'em; wpisy poprzedniego dnia sa sprzatanie przy flushu.
//
// Retencja przy awarii: nieudany flush NIE rusza watermarka, wiec delty sa
// retencjonowane BEZ LIMITU az do skutecznego zapisu (swiadoma decyzja —
// enforcement ma dalej widziec pelne zuzycie read-your-writes, a pamiec jest
// ograniczona liczba roznych kluczy dziennych, nie tempem requestow).
//
// Wspolbieznosc: kazdy zapis klucza (leniwy baseline + inkrement) dzieje sie w
// JEDNEJ sekcji krytycznej, wiec ewikcja zerowych delt po flushu nigdy nie
// usunie wpisu miedzy jego utworzeniem a pierwszym inkrementem.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use parking_lot::{Mutex, RwLock};

use crate::db::models::TokenQuota;
use crate::db::DbPool;

/// Klucz wiersza `token_usage_daily` (`usage:{node}:{org}:{user}:{model}:{day}`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct UsageKey {
    pub node_id: String,
    pub org_id: String,
    pub user_id: String,
    pub model_id: String,
    pub day: String,
}

/// Sumy licznikow jednego klucza. `total_tokens` NIE jest przechowywany —
/// zawsze `prompt + completion` (embeddingi i audio licza sie osobno).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct UsageTotals {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub embedding_tokens: i64,
    pub audio_ms: i64,
    pub request_count: i64,
}

impl UsageTotals {
    fn total_tokens(&self) -> i64 {
        self.prompt_tokens + self.completion_tokens
    }

    fn is_zero(&self) -> bool {
        self.prompt_tokens == 0
            && self.completion_tokens == 0
            && self.embedding_tokens == 0
            && self.audio_ms == 0
            && self.request_count == 0
    }

    /// Delta względem watermarka (per pole); liczniki tylko rosną, więc wynik
    /// jest nieujemny.
    fn since(&self, watermark: &UsageTotals) -> UsageTotals {
        UsageTotals {
            prompt_tokens: self.prompt_tokens - watermark.prompt_tokens,
            completion_tokens: self.completion_tokens - watermark.completion_tokens,
            embedding_tokens: self.embedding_tokens - watermark.embedding_tokens,
            audio_ms: self.audio_ms - watermark.audio_ms,
            request_count: self.request_count - watermark.request_count,
        }
    }
}

/// Stan jednego klucza w overlayu: absolutny stan procesu + ile z niego
/// zostalo juz zapisane do bazy.
#[derive(Debug, Clone, Copy)]
struct OverlayEntry {
    totals: UsageTotals,
    watermark: UsageTotals,
}

impl OverlayEntry {
    fn unflushed(&self) -> UsageTotals {
        self.totals.since(&self.watermark)
    }
}

/// Zagregowany overlay dla GUI (wynik `overlay_usage_summary`).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct OverlaySummaryRow {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub audio_ms: i64,
    pub embedding_tokens: i64,
}

/// Cache zuzycia jednego procesu. Pula do baseline'ow i odczytu czlonkow grup
/// siedzi pod `RwLock`, bo moze dotarc pozniej niz pierwsze uzycie cache'a
/// (kolejnosc initow w bootstrapie nie musi byc gwarantowana). `None` oznacza
/// tryb bez bazy (testy / fallback przed initem): baseline zero, grupy puste.
///
/// Mutex jest z parking_lot (bez poisoning): panic jednego watka pod lockiem
/// nie zatruwa cache'a na stałe.
pub(crate) struct TokenUsageCache {
    db: RwLock<Option<DbPool>>,
    entries: Mutex<HashMap<UsageKey, OverlayEntry>>,
}

static CACHE: OnceLock<Arc<TokenUsageCache>> = OnceLock::new();

/// Inicjalizuje GLOBALNY cache pula do baseline'ow. Idempotentne i ODPORNE NA
/// KOLEJNOSC: nawet gdy instancja globalna powstala wczesniej jako fallback bez
/// bazy (np. petla metrics-workera zdazyła cos zapisac przed initem), pula i
/// tak trafia do TEJ SAMEJ instancji (`set_db` nadpisuje bezwarunkowo — ostatni
/// init wygrywa, w produkcji inicjalizator jest jeden).
pub fn init_token_usage_cache(pool: DbPool) {
    global().set_db(pool);
}

/// Globalna instancja procesu (fallback bez DB, dopoki `init_token_usage_cache`
/// nie zalezci). `pub(crate)` dla petli metrics-workera i testow.
pub(crate) fn global() -> &'static Arc<TokenUsageCache> {
    CACHE.get_or_init(|| Arc::new(TokenUsageCache::new(None)))
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

impl TokenUsageCache {
    pub(crate) fn new(db: Option<DbPool>) -> Self {
        Self {
            db: RwLock::new(db),
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Klon Arc puli (tani) — czytane z pod RwLock, bo pula moze dotrzec po
    /// utworzeniu instancji.
    fn database(&self) -> Option<DbPool> {
        self.db.read().clone()
    }

    /// Podpina pulę do instancji. Nadpisuje bezwarunkowo — dzięki temu
    /// `init_token_usage_cache` nigdy nie przegrywa po cichu z fallbackiem
    /// utworzonym wcześniej przez `global()`.
    pub(crate) fn set_db(&self, pool: DbPool) {
        *self.db.write() = Some(pool);
    }

    /// Leniwy baseline: stan ABSOLUTNY klucza = wiersz z bazy + przyszle
    /// inkrementy. Wywolywany TYLKO z sekcji krytycznej `record` (krótki SELECT
    /// po kluczu głównym pod trzymanym mutexem), raz na zycie klucza.
    fn load_baseline(&self, key: &UsageKey) -> OverlayEntry {
        let zeros = OverlayEntry {
            totals: UsageTotals::default(),
            watermark: UsageTotals::default(),
        };
        let Some(db) = self.database() else {
            return zeros;
        };
        let Ok(conn) = db.read() else {
            return zeros;
        };
        let id = crate::db::repository::token_usage_id(
            &key.node_id,
            &key.org_id,
            &key.user_id,
            &key.model_id,
            &key.day,
        );
        let Ok(mut stmt) = conn.prepare_cached(
            "SELECT prompt_tokens, completion_tokens, embedding_tokens, audio_ms, \
             request_count FROM token_usage_daily WHERE id = ?1",
        ) else {
            return zeros;
        };
        match stmt.query_row(rusqlite::params![id], |row| {
            Ok(UsageTotals {
                prompt_tokens: row.get(0)?,
                completion_tokens: row.get(1)?,
                embedding_tokens: row.get(2)?,
                audio_ms: row.get(3)?,
                request_count: row.get(4)?,
            })
        }) {
            Ok(totals) => {
                // Watermark startuje NA POZIOMIE baseline'u: baza juz te liczby
                // zawiera, wiec delta do flusha/enforcementu to tylko nasze
                // przyszle inkrementy.
                OverlayEntry {
                    totals,
                    watermark: totals,
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => zeros,
            Err(e) => {
                tracing::warn!(error = %e, "odczyt baseline'u token_usage_daily nieudany");
                zeros
            }
        }
    }

    /// JEDNA sekcja krytyczna na (ewentualny baseline + inkrement): bez tego
    /// ewikcja zerowych delt po flushu moglaby usunac swiezo wstawiony wpis
    /// pomiedzy jego utworzeniem a pierwszym inkrementem i po cichu zgubic go.
    fn record<F: FnOnce(&mut UsageTotals)>(
        &self,
        node_id: &str,
        org_id: &str,
        user_id: &str,
        model_id: &str,
        day: &str,
        apply: F,
    ) {
        let key = UsageKey {
            node_id: node_id.to_string(),
            org_id: org_id.to_string(),
            user_id: user_id.to_string(),
            model_id: model_id.to_string(),
            day: day.to_string(),
        };
        let mut guard = self.entries.lock();
        if !guard.contains_key(&key) {
            let baseline = self.load_baseline(&key);
            guard.insert(key.clone(), baseline);
        }
        if let Some(entry) = guard.get_mut(&key) {
            apply(&mut entry.totals);
        }
    }

    /// Jeden skonczony call chat: prompt/completion + jedno zadanie.
    pub(crate) fn record_chat_usage_on(
        &self,
        node_id: &str,
        org_id: &str,
        user_id: &str,
        model_id: &str,
        day: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) {
        self.record(node_id, org_id, user_id, model_id, day, |t| {
            t.prompt_tokens += prompt_tokens;
            t.completion_tokens += completion_tokens;
            t.request_count += 1;
        });
    }

    /// Jedno zapytanie embeddingowe: tokeny do osobnego licznika + jedno zadanie.
    pub(crate) fn record_embedding_tokens_on(
        &self,
        node_id: &str,
        org_id: &str,
        user_id: &str,
        model_id: &str,
        day: &str,
        embedding_tokens: i64,
    ) {
        self.record(node_id, org_id, user_id, model_id, day, |t| {
            t.embedding_tokens += embedding_tokens;
            t.request_count += 1;
        });
    }

    /// Jedno przetworzenie STT: milisekundy audio + jedno zadanie.
    pub(crate) fn record_audio_ms_on(
        &self,
        node_id: &str,
        org_id: &str,
        user_id: &str,
        model_id: &str,
        day: &str,
        audio_ms: i64,
    ) {
        self.record(node_id, org_id, user_id, model_id, day, |t| {
            t.audio_ms += audio_ms;
            t.request_count += 1;
        });
    }

    /// Czlonkowie grupy — jedno zapytanie na tick (scope 'group' sumuje po
    /// user_id zamiast filtrowac SQL-em).
    fn group_member_ids(&self, group_id: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        let Some(db) = self.database() else {
            return out;
        };
        let Ok(conn) = db.read() else {
            return out;
        };
        let Ok(mut stmt) =
            conn.prepare_cached("SELECT user_id FROM group_members WHERE group_id = ?1")
        else {
            return out;
        };
        match stmt.query_map(rusqlite::params![group_id], |row| row.get::<_, String>(0)) {
            Ok(rows) => {
                for id in rows.flatten() {
                    out.insert(id);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "odczyt czlonkow grupy dla overlay nieudany");
            }
        }
        out
    }

    /// Niedoliczona (niesflushowana) czesc zuzycia pasujaca do limitu — doklada
    /// ja koordynator dzierzaw do wyniku `global/node_usage_for_quota`, zeby
    /// enforcement widzial zapisy natychmiast (read-your-writes).
    pub(crate) fn overlay_usage_for_quota_impl(
        &self,
        quota: &TokenQuota,
        period_key: &str,
        node: Option<&str>,
    ) -> i64 {
        let members: Option<HashSet<String>> = if quota.scope_type == "group" {
            quota
                .subject_id
                .as_deref()
                .map(|g| self.group_member_ids(g))
        } else {
            None
        };
        let guard = self.entries.lock();
        let mut total = 0i64;
        for (key, entry) in guard.iter() {
            if key.org_id != quota.org_id {
                continue;
            }
            let period_matches = if quota.period == "monthly" {
                key.day.starts_with(period_key)
            } else {
                key.day == period_key
            };
            if !period_matches {
                continue;
            }
            if let Some(node) = node {
                if key.node_id != node {
                    continue;
                }
            }
            let subject_match = match (quota.scope_type.as_str(), quota.subject_id.as_deref()) {
                ("user", Some(id)) => &key.user_id == id,
                ("model", Some(id)) => &key.model_id == id,
                ("group", Some(_)) => members.as_ref().is_some_and(|m| m.contains(&key.user_id)),
                ("org", _) => true,
                _ => false,
            };
            if !subject_match {
                continue;
            }
            // Dodatkowe ograniczenie modelu limitu (poza scope='model', ktore juz
            // wiazalo subject_id na modelu).
            if let Some(model) = quota.model_id.as_deref() {
                if quota.scope_type != "model" && key.model_id != model {
                    continue;
                }
            }
            total += entry.unflushed().total_tokens();
        }
        total
    }

    /// Overlay zagregowany tak jak `repository::usage_summary` (po user/model/
    /// day), zeby GUI doliczylo niesflushowana czesc do wierszy sumarycznych.
    pub(crate) fn overlay_usage_summary_impl(
        &self,
        org_id: &str,
        period_key: &str,
        monthly: bool,
        group_by: &str,
    ) -> HashMap<String, OverlaySummaryRow> {
        let guard = self.entries.lock();
        let mut out: HashMap<String, OverlaySummaryRow> = HashMap::new();
        for (key, entry) in guard.iter() {
            if key.org_id != org_id {
                continue;
            }
            let period_matches = if monthly {
                key.day.starts_with(period_key)
            } else {
                key.day == period_key
            };
            if !period_matches {
                continue;
            }
            let delta = entry.unflushed();
            if delta.is_zero() {
                continue;
            }
            let dim: &str = match group_by {
                "model" => key.model_id.as_str(),
                "day" => key.day.as_str(),
                _ => key.user_id.as_str(),
            };
            let row = out.entry(dim.to_string()).or_default();
            row.prompt_tokens += delta.prompt_tokens;
            row.completion_tokens += delta.completion_tokens;
            row.total_tokens += delta.total_tokens();
            row.request_count += delta.request_count;
            row.audio_ms += delta.audio_ms;
            row.embedding_tokens += delta.embedding_tokens;
        }
        out
    }

    /// Pobiera delty do flusha — DESTRUKCYJNIE: watermark przesuwa się do
    /// `totals` POD mutexem, więc ta sama delta nie moze zostac zapisana dwukrotnie
    /// przez nachodzace na siebie snapshoty. Nieudany flush COFA zmiany przez
    /// `restore_usage_delta` (retencja bez limitu az do skutecznego zapisu).
    /// Wpis z zerowa delta pozostaje w mapie az do ewikcji po udanym COMMIT-cie,
    /// wiec restore zawsze znajduje swoj wpis.
    pub(crate) fn take_usage_delta(&self) -> Vec<(UsageKey, UsageTotals)> {
        let mut guard = self.entries.lock();
        let mut out = Vec::new();
        for (key, entry) in guard.iter_mut() {
            let delta = entry.unflushed();
            if delta.is_zero() {
                continue;
            }
            entry.watermark = entry.totals;
            out.push((key.clone(), delta));
        }
        out
    }

    /// Cofa watermark o NIEZAPISANA partie (flush nieudany) — nastepny cykl
    /// widzi te delty ponownie. Wywoluje wylacznie jedyny konsument partii
    /// (petla metrics-workera), wiec wpisy nie moga zostac miedzy czasem usuniete.
    pub(crate) fn restore_usage_delta(&self, deltas: &[(UsageKey, UsageTotals)]) {
        let mut guard = self.entries.lock();
        for (key, delta) in deltas {
            if let Some(entry) = guard.get_mut(key) {
                entry.watermark.prompt_tokens -= delta.prompt_tokens;
                entry.watermark.completion_tokens -= delta.completion_tokens;
                entry.watermark.embedding_tokens -= delta.embedding_tokens;
                entry.watermark.audio_ms -= delta.audio_ms;
                entry.watermark.request_count -= delta.request_count;
            }
        }
    }

    /// Po UDANYM COMMIT-cie usuwa wpisy w pelni zapisane (lazy eviction starych
    /// dni i wypłukanych kluczy). Klucze niesflushowane (przywrócone po błędzie
    /// albo z nowymi inkrementami po take) maja niezerowa delte i zostaja.
    pub(crate) fn evict_persisted<'a>(&self, keys: impl ExactSizeIterator<Item = &'a UsageKey>) {
        let mut guard = self.entries.lock();
        for key in keys {
            if let Some(entry) = guard.get(key) {
                if entry.totals == entry.watermark {
                    guard.remove(key);
                }
            }
        }
    }

    /// Podglad delt BEZ ich pobierania (testy asercji po flushu musza widziec
    /// stan, nie go konsumować).
    #[cfg(test)]
    pub(crate) fn pending_deltas_snapshot(&self) -> Vec<(UsageKey, UsageTotals)> {
        let guard = self.entries.lock();
        guard
            .iter()
            .filter_map(|(key, entry)| {
                let delta = entry.unflushed();
                (!delta.is_zero()).then(|| (key.clone(), delta))
            })
            .collect()
    }
}

/// Globalne wrappery — callerzy na sciezce requestu nie dotykaja instancji.
pub fn record_chat_usage(
    node_id: &str,
    org_id: &str,
    user_id: &str,
    model_id: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
) {
    let day = today();
    global().record_chat_usage_on(
        node_id,
        org_id,
        user_id,
        model_id,
        &day,
        prompt_tokens,
        completion_tokens,
    );
}

pub fn record_embedding_tokens(
    node_id: &str,
    org_id: &str,
    user_id: &str,
    model_id: &str,
    embedding_tokens: i64,
) {
    let day = today();
    global().record_embedding_tokens_on(node_id, org_id, user_id, model_id, &day, embedding_tokens);
}

pub fn record_audio_ms(node_id: &str, org_id: &str, user_id: &str, model_id: &str, audio_ms: i64) {
    let day = today();
    global().record_audio_ms_on(node_id, org_id, user_id, model_id, &day, audio_ms);
}

pub(crate) fn overlay_usage_for_quota(
    quota: &TokenQuota,
    period_key: &str,
    node: Option<&str>,
) -> i64 {
    global().overlay_usage_for_quota_impl(quota, period_key, node)
}

pub(crate) fn overlay_usage_summary(
    org_id: &str,
    period_key: &str,
    monthly: bool,
    group_by: &str,
) -> HashMap<String, OverlaySummaryRow> {
    global().overlay_usage_summary_impl(org_id, period_key, monthly, group_by)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::org::DEFAULT_ORG_ID;

    const NODE: &str = "node-cache-test";

    fn pool() -> DbPool {
        crate::db::init(std::path::Path::new(":memory:")).expect("init test db")
    }

    fn quota(
        scope_type: &str,
        subject: Option<&str>,
        model: Option<&str>,
        period: &str,
    ) -> TokenQuota {
        TokenQuota {
            id: format!("quota-test-{scope_type}"),
            org_id: DEFAULT_ORG_ID.to_string(),
            scope_type: scope_type.to_string(),
            subject_id: subject.map(str::to_string),
            model_id: model.map(str::to_string),
            period: period.to_string(),
            max_total_tokens: 100_000,
            is_active: true,
            created_at: "2026-08-23T00:00:00Z".to_string(),
        }
    }

    /// Symuluje usage-czesc flusha workera: take → zapis delt w jednej
    /// transakcji → ewikcja w pelni zapisanych wpisów.
    fn flush_usage(cache: &TokenUsageCache, db: &DbPool) {
        let deltas = cache.take_usage_delta();
        assert!(!deltas.is_empty(), "flush bez delt nie ma sensu");
        crate::db::repository::with_writer_tx(db, |tx| {
            for (key, totals) in &deltas {
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
            Ok(())
        })
        .expect("flush tx ok");
        cache.evict_persisted(deltas.iter().map(|(k, _)| k));
    }

    fn summary_row(db: &DbPool, user: &str, day: &str) -> crate::db::models::UsageSummaryRow {
        crate::db::repository::usage_summary(db, DEFAULT_ORG_ID, "daily", day, "user")
            .expect("usage_summary")
            .into_iter()
            .find(|r| r.key == user)
            .expect("wiersz uzytkownika")
    }

    /// Zapis do cache jest natychmiastowy i NIE dotyka bazy: read-your-writes
    /// bez synchronicznego UPSERT-a.
    #[test]
    fn record_is_visible_immediately_without_db_write() {
        let db = pool();
        let cache = TokenUsageCache::new(Some(db.clone()));
        let day = "2026-08-23";

        cache.record_chat_usage_on(NODE, DEFAULT_ORG_ID, "u1", "m1", day, 10, 5);
        let q = quota("user", Some("u1"), None, "daily");
        assert_eq!(
            cache.overlay_usage_for_quota_impl(&q, day, Some(NODE)),
            15,
            "enforcement widzi zapis od razu"
        );
        assert!(
            crate::db::repository::usage_summary(&db, DEFAULT_ORG_ID, "daily", day, "user")
                .expect("summary")
                .is_empty(),
            "baza nie moze zostac zapisana przez record_*"
        );

        cache.record_chat_usage_on(NODE, DEFAULT_ORG_ID, "u1", "m1", day, 7, 3);
        assert_eq!(cache.overlay_usage_for_quota_impl(&q, day, Some(NODE)), 25);
    }

    /// Baseline pierwszego zapisu danego klucza jest leniwie wczytany z bazy:
    /// overlay dolicza TYLKO niesflushowany przyrost (nie dubluje istniejacego
    /// stanu wiersza).
    #[test]
    fn baseline_seeded_from_existing_day_row() {
        let db = pool();
        let day = "2026-08-23";
        crate::db::repository::bump_token_usage(
            &db,
            NODE,
            DEFAULT_ORG_ID,
            "u1",
            "m1",
            day,
            300,
            200,
        )
        .expect("seed wiersza");

        let cache = TokenUsageCache::new(Some(db.clone()));
        cache.record_chat_usage_on(NODE, DEFAULT_ORG_ID, "u1", "m1", day, 100, 50);

        let q = quota("user", Some("u1"), None, "daily");
        assert_eq!(
            cache.overlay_usage_for_quota_impl(&q, day, Some(NODE)),
            150,
            "overlay liczy tylko przyrost ponad baseline"
        );

        flush_usage(&cache, &db);
        let row = summary_row(&db, "u1", day);
        assert_eq!(row.total_tokens, 650, "baza: 500 baseline + 150 delty");
        assert_eq!(row.request_count, 2);
        assert_eq!(
            cache.overlay_usage_for_quota_impl(&q, day, Some(NODE)),
            0,
            "po flushu overlay niczego nie doklada"
        );
    }

    /// Flush zapisuje tylko delte, a commit zeruje widocznosc delt i usuwa
    /// wypłukane wpisy (lazy eviction).
    #[test]
    fn flush_persists_only_delta_and_zeroes_evict() {
        let db = pool();
        let cache = TokenUsageCache::new(Some(db.clone()));
        let day = "2026-08-23";
        cache.record_audio_ms_on(NODE, DEFAULT_ORG_ID, "u2", "whisper", day, 1_000);

        flush_usage(&cache, &db);
        assert_eq!(summary_row(&db, "u2", day).audio_ms, 1_000);
        assert!(
            cache.pending_deltas_snapshot().is_empty(),
            "watermark = snapshot → brak delt do ponownego flusha"
        );

        // Nowy przyrost po flushu startuje od przetrzymywanej pozycji watermarka.
        cache.record_audio_ms_on(NODE, DEFAULT_ORG_ID, "u2", "whisper", day, 500);
        flush_usage(&cache, &db);
        assert_eq!(summary_row(&db, "u2", day).audio_ms, 1_500);
        assert!(cache.pending_deltas_snapshot().is_empty());
    }

    /// Rollover dobowy: nowy dzien to nowe klucze z wlasnym baseline'em, bez
    /// zadnego specjalnego kodu — overlay per okres liczy tylko swoje dni.
    #[test]
    fn day_rollover_starts_fresh_key_with_own_baseline() {
        let db = pool();
        let cache = TokenUsageCache::new(Some(db.clone()));
        cache.record_chat_usage_on(NODE, DEFAULT_ORG_ID, "u3", "m1", "2026-08-22", 50, 50);
        flush_usage(&cache, &db);
        cache.record_chat_usage_on(NODE, DEFAULT_ORG_ID, "u3", "m1", "2026-08-23", 10, 5);

        let q = quota("user", Some("u3"), None, "daily");
        assert_eq!(
            cache.overlay_usage_for_quota_impl(&q, "2026-08-22", Some(NODE)),
            0
        );
        assert_eq!(
            cache.overlay_usage_for_quota_impl(&q, "2026-08-23", Some(NODE)),
            15
        );

        let month = quota("user", Some("u3"), None, "monthly");
        assert_eq!(
            cache.overlay_usage_for_quota_impl(&month, "2026-08", Some(NODE)),
            15,
            "scope miesieczny sumuje niesflushowane dni prefiksu"
        );
    }

    /// Scope'y user/model/org filtruja wlasciwe pola klucza.
    #[test]
    fn overlay_sums_user_model_and_org_scopes() {
        let cache = TokenUsageCache::new(None);
        let day = "2026-08-23";
        cache.record_chat_usage_on(NODE, DEFAULT_ORG_ID, "u1", "m1", day, 10, 0);
        cache.record_chat_usage_on(NODE, DEFAULT_ORG_ID, "u2", "m1", day, 20, 0);
        cache.record_chat_usage_on(NODE, DEFAULT_ORG_ID, "u1", "m2", day, 40, 0);
        cache.record_chat_usage_on(NODE, "inny-org", "u1", "m1", day, 999, 0);

        let by_user = quota("user", Some("u1"), None, "daily");
        assert_eq!(cache.overlay_usage_for_quota_impl(&by_user, day, None), 50);
        let by_model = quota("model", Some("m1"), None, "daily");
        assert_eq!(cache.overlay_usage_for_quota_impl(&by_model, day, None), 30);
        let by_org = quota("org", None, None, "daily");
        assert_eq!(cache.overlay_usage_for_quota_impl(&by_org, day, None), 70);
        let restricted = quota("org", None, Some("m2"), "daily");
        assert_eq!(
            cache.overlay_usage_for_quota_impl(&restricted, day, None),
            40
        );
        // Filtr wezla: overlay innego noda nie wchodzi do dzierzawy tego noda.
        assert_eq!(
            cache.overlay_usage_for_quota_impl(&by_user, day, Some("other-node")),
            0
        );
    }

    /// Scope 'group' sumuje czlonkow grupy wczytanych jednym zapytaniem z bazy.
    #[test]
    fn group_scope_sums_members_from_db() {
        let db = pool();
        // group_members ma FK na user_accounts — czlonkowie musza istniec.
        let mut member_ids = Vec::new();
        for username in ["u1-group-test", "u2-group-test"] {
            member_ids.push(
                crate::db::repository::create_user_account(
                    &db,
                    username,
                    "hash",
                    username,
                    &format!("{username}@example.com"),
                )
                .expect("create user"),
            );
        }
        let group_id =
            crate::db::repository::create_group(&db, "grupa-test", "opis").expect("create group");
        for id in &member_ids {
            crate::db::repository::add_user_to_group(&db, &group_id, id).expect("member");
        }

        let cache = TokenUsageCache::new(Some(db));
        let day = "2026-08-23";
        let [u1, u2] = [member_ids[0].as_str(), member_ids[1].as_str()];
        cache.record_chat_usage_on(NODE, DEFAULT_ORG_ID, u1, "m1", day, 30, 0);
        cache.record_chat_usage_on(NODE, DEFAULT_ORG_ID, u2, "m1", day, 20, 0);
        cache.record_chat_usage_on(NODE, DEFAULT_ORG_ID, "u3-nie-czlonek", "m1", day, 777, 0);

        let by_group = quota("group", Some(&group_id), None, "daily");
        assert_eq!(
            cache.overlay_usage_for_quota_impl(&by_group, day, Some(NODE)),
            50,
            "tylko czlonkowie grupy (u3 nie nalezy)"
        );
    }

    /// Regresja CR-001: instancja utworzona fallbackowo BEZ bazy musi po
    /// `set_db` (droga `init_token_usage_cache`) faktycznie dostac pule —
    /// cichy przegrywajacy `OnceLock::set` sprawialby, że baseline i scope
    /// 'group' byłyby na zawsze martwe.
    #[test]
    fn init_wires_db_into_fallback_instance_regardless_of_order() {
        let db = pool();
        let cache = TokenUsageCache::new(None);
        assert!(cache.database().is_none(), "fallback startuje bez puli");

        // Dokladnie to, co robi init_token_usage_cache na instancji globalnej.
        cache.set_db(db.clone());
        assert!(
            cache.database().is_some(),
            "pula musi trafic do istniejacej instancji"
        );

        // Podpieta pula dziala: baseline wczytany z zasianego wiersza.
        let day = "2026-08-23";
        crate::db::repository::bump_token_usage(
            &db,
            NODE,
            DEFAULT_ORG_ID,
            "u-init",
            "m1",
            day,
            300,
            200,
        )
        .expect("seed wiersza");
        cache.record_chat_usage_on(NODE, DEFAULT_ORG_ID, "u-init", "m1", day, 100, 50);

        let q = quota("user", Some("u-init"), None, "daily");
        assert_eq!(
            cache.overlay_usage_for_quota_impl(&q, day, None),
            150,
            "overlay liczy tylko przyrost ponad baseline z podpietej puli"
        );
    }

    /// Regresja CR-002: zapisy rownolegle z cyklem take+evict nie moga zgubic
    /// ani jednego inkrementu. Stary kod (dwie sekcje locka w record) tracil
    /// wpisy miedzy INSERT-em baseline'u a apply; niedestrukcyjny drain z
    /// podwojnym commitem nakladal watermark wielokrotnie.
    #[test]
    fn concurrent_record_and_evict_never_lose_increments() {
        const WRITERS: usize = 4;
        const WRITES_PER_WRITER: usize = 500;

        let cache = Arc::new(TokenUsageCache::new(None));
        let writers_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut handles = Vec::new();
        for w in 0..WRITERS {
            let cache = cache.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..WRITES_PER_WRITER {
                    cache.record_chat_usage_on(
                        NODE,
                        DEFAULT_ORG_ID,
                        "u-race",
                        &format!("m{w}"),
                        "2026-08-23",
                        1,
                        0,
                    );
                }
            }));
        }
        // Flusher zabiera delty i "zapisuje" je (ewikcja po udanym flushu) tak
        // czesto, jak tylko moze — dokladnie ten ruch, ktory otwieral okna na
        // utrate w poprzednich wariantach. Po zakonczeniu writerow dociaga reszte.
        let flusher_cache = cache.clone();
        let flushed = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let flushed_counter = flushed.clone();
        let done_flag = writers_done.clone();
        let flusher = std::thread::spawn(move || loop {
            let deltas = flusher_cache.take_usage_delta();
            if !deltas.is_empty() {
                for (_, totals) in &deltas {
                    flushed_counter
                        .fetch_add(totals.prompt_tokens, std::sync::atomic::Ordering::Relaxed);
                }
                flusher_cache.evict_persisted(deltas.iter().map(|(k, _)| k));
            }
            if done_flag.load(std::sync::atomic::Ordering::Acquire)
                && flusher_cache.pending_deltas_snapshot().is_empty()
            {
                return;
            }
            std::thread::yield_now();
        });
        for handle in handles {
            handle.join().expect("writer nie moze panicować");
        }
        writers_done.store(true, std::sync::atomic::Ordering::Release);
        flusher.join().expect("flusher nie moze panicować");

        assert_eq!(
            flushed.load(std::sync::atomic::Ordering::Relaxed),
            (WRITERS * WRITES_PER_WRITER) as i64,
            "kazdy inkrement musi przejsc przez take dokladnie raz"
        );
    }
}
