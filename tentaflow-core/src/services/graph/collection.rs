// ===== Plik: services/graph/collection.rs — rejestr kolekcji grafowych per (org, addon) =====
//
// Lustro `vector::namespace::NamespaceManager` dla grafu. Trzyma proces-szeroki
// cache `(org_id, addon_id, collection) -> Arc<GraphEntry>` (DashMap, lock-free
// odczyt na hot-path), gdzie każda otwarta kolekcja odpowiada wierszowi w
// `addon_graph_collections` (PK `(org_id, addon_id, collection)`) i plikowi
// `.cozo` pod `<HOME>/.tentaflow/orgs/<org>/addons/<addon>/graph/<collection>.cozo`.
//
// Izolacja multi-tenant: lookup ZAWSZE po `(org_id, addon_id, collection)`;
// org_id wchodzi w każdy SELECT/INSERT/UPDATE/DELETE i w rozwiązanie ścieżki
// pliku, więc ten sam `addon_id` w dwóch organizacjach pisze do osobnych plików.
//
// MANAGER-OWNED LIFETIME (runda 2 codex pkt A): `GraphManager` NIGDY nie wydaje
// `Arc<CozoBackend>` na zewnątrz. Każdy wpis cache trzyma backend za
// `RwLock<Option<CozoBackend>>` (`Option`, bo eviction/delete wyjmuje i drop'uje
// backend pod write-lockiem). Wszystkie operacje grafowe (upsert/query/neighbors/
// count/export) wykonują się WEWNĄTRZ managera, trzymając per-kolekcyjny lock —
// caller dostaje WYNIK, nie uchwyt. To naraz:
//   - czyni quota check+mutate ATOMOWYM (write-lock obejmuje count i mutację Cozo
//     — dwóch równoległych piszących NIE przekroczy limitu, bug #4),
//   - czyni delete/eviction bezpiecznym: write-lock → `take()` backend → drop pod
//     lockiem (sled flush+close) → remove z mapy → kasuj pliki. Brak operacji w
//     locie, brak kasowania pod żywym uchwytem (bug #5),
//   - czyni `MAX_OPEN_GRAPHS` PRAWDZIWYM limitem otwartych baz sled, bo eviction
//     realnie zamyka backend (nie ma zewnętrznego `Arc`, który by go trzymał, bug #3).
//
// MODEL LICZNIKÓW (runda 3 codex bug F): Cozo jest źródłem prawdy dla GRAFU
// (liczba realnych węzłów/krawędzi), a kolumny `node_count`/`edge_count` w
// `addon_graph_collections` to ATOMOWY LEDGER REZERWACJI QUOTY per (org, addon).
// Globalna quota per-addon sumuje się MIĘDZY kolekcjami, więc nie da się jej
// wyegzekwować samym per-kolekcyjnym lockiem — dwóch piszących do RÓŻNYCH
// kolekcji tego samego addona musi konkurować o jeden globalny licznik. Robi to
// transakcja `BEGIN IMMEDIATE` (jeden writer SQLite): `SELECT SUM(node_count)
// WHERE org,addon` → jeśli `+delta > limit` reject → `UPDATE node_count += delta
// WHERE collection` → COMMIT (rezerwacja). Dopiero potem mutacja Cozo; gdy Cozo
// padnie, kompensujemy `node_count -= delta` (zwolnienie rezerwacji). Dryf między
// ledgerem a Cozo (np. po crashu między rezerwacją a mutacją) koryguje
// `reconcile_counts` przy otwarciu kolekcji — ustawia licznik na realny count z
// Cozo. Ledger może chwilowo przeszacować (rezerwacja bez mutacji), nigdy nie
// niedoszacuje pod żywym ruchem, więc quota jest bezpieczna (fail-safe w stronę
// odrzucenia).
//
// Ograniczenie pamięci: sled bierze 1 GiB cache + flush 500ms na KAŻDĄ otwartą
// bazę i Cozo NIE przepuszcza tuningu (patrz backend.rs), więc manager trzyma
// lazy-open + LRU eviction: cap `MAX_OPEN_GRAPHS` jednocześnie otwartych
// backendów, eviction najdawniej używanego (LRU po `last_used`). Eviction zamyka
// backend (drop pod write-lockiem); dane na dysku zostają, następny dostęp je
// odtworzy.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use dashmap::DashMap;

use super::backend::{CozoBackend, GraphBackend, GraphEngine};
use super::csr::Csr;
use super::error::{GraphError, Result};
use crate::db::DbPool;
use crate::services::vector::namespace::{
    validate_addon_id, validate_namespace_name, validate_org_id,
};

/// Twardy limit kolekcji grafowych na (org, addon). Każda otwarta kolekcja to
/// osobny plik Cozo + uchwyt — trzymamy modnie jak vector (10 namespaces).
pub const MAX_COLLECTIONS_PER_ADDON: u32 = 10;

/// Twardy limit węzłów na (org, addon) (sumarycznie po kolekcjach). Domyślny
/// pułap, gdy `addon_resource_limits.graph_nodes_max` jest 0 (nieustawiony).
pub const MAX_NODES_PER_ADDON: u64 = 1_000_000;

/// Twardy limit krawędzi na (org, addon) (sumarycznie po kolekcjach). Domyślny
/// pułap, gdy `addon_resource_limits.graph_edges_max` jest 0 (nieustawiony).
pub const MAX_EDGES_PER_ADDON: u64 = 5_000_000;

/// Maksymalna liczba JEDNOCZEŚNIE otwartych backendów `CozoBackend` w cache.
/// Ponad próg manager zamyka najdawniej używany (LRU) — każdy otwarty sled to
/// realny narzut pamięci (cache 1 GiB default, nietuningowalny przez Cozo). Na
/// telefonie (Android/iOS) próg jest niższy: pamięć urządzenia jest dużo mniejsza,
/// a kilka otwartych baz sled po ~1 GiB cache od razu by ją wysyciło.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub const MAX_OPEN_GRAPHS: usize = 4;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub const MAX_OPEN_GRAPHS: usize = 16;

/// Maksymalna liczba prób re-fetch w pętli `with_read`/`with_write` zanim
/// wymusimy otwarcie kanonicznego wpisu BEZ eviction (gwarancja postępu).
/// Pod presją (aktywny zbiór kluczy > `MAX_OPEN_GRAPHS`) caller mógłby w kółko
/// trafiać na wpis wyeksmitowany tuż przed użyciem (starvation/livelock); po
/// wyczerpaniu prób akceptujemy CHWILOWY over-cap, byle zagwarantować progres.
const MAX_REFETCH_ATTEMPTS: u32 = 64;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct GraphKey {
    org_id: String,
    addon_id: String,
    collection: String,
}

/// Stan otwarcia backendu w obrębie jednego wpisu. Przejścia chronione
/// write-lockiem wpisu, więc open NIE może się zdublować (dwóch równoległych
/// openerów tej samej kolekcji serializuje się na write-locku — sled bierze
/// wyłączny lock pliku, dwa równoległe `sled::open` na tym samym katalogu
/// skończyłyby się `WouldBlock`).
///
/// `Removed` to STAN TERMINALNY wpisu (runda 3 codex bug G/H): wpis został
/// wyeksmitowany (eviction) albo skasowany (delete). Po wejściu w `Removed`
/// backend jest zamknięty i wpis NIE może już otworzyć bazy — żaden
/// przeterminowany `Arc<GraphEntry>` trzymany przez inny wątek nie wskrzesi
/// usuniętej/wyeksmitowanej bazy. Wątek, który widzi `Removed`, re-fetchuje
/// kanoniczny wpis z mapy (`entry_get*`) i ponawia. Eviction i delete różnią się
/// tylko tym, czy zostaje wiersz DB: eviction zostawia (re-fetch reotwiera ten
/// sam plik), delete kasuje (re-fetch widzi brak wiersza → świeża kolekcja).
enum BackendSlot {
    Closed,
    Open(CozoBackend),
    Removed,
}

/// Wynik leniwego otwarcia backendu (`ensure_open`). Rozróżnia, czy ten wątek
/// realnie otworzył bazę, zastał ją już otwartą, czy wpis jest terminalnie
/// `Removed` (wymaga re-fetch w pętli wołającego).
enum OpenOutcome {
    Opened,
    AlreadyOpen,
    Removed,
}

/// Wpis cache: leniwie otwierany backend za `RwLock` + dane do (ponownego)
/// otwarcia + znacznik LRU. Backend jest otwierany dopiero w `with_read`/
/// `with_write` pod write-lockiem (dedup open). `engine`/`file_path` są niemienne
/// po utworzeniu wpisu — pozwalają odtworzyć backend po eviction.
struct GraphEntry {
    slot: RwLock<BackendSlot>,
    engine: GraphEngine,
    file_path: PathBuf,
    /// Monotoniczny znacznik ostatniego dostępu (z `GraphManager::clock`) —
    /// najmniejszy = najdawniej używany, kandydat do eviction.
    last_used: AtomicU64,
}

pub struct GraphManager {
    pool: DbPool,
    collections: DashMap<GraphKey, Arc<GraphEntry>>,
    /// Logiczny zegar LRU — inkrementowany przy każdym dostępie do wpisu.
    clock: AtomicU64,
    /// Liczba aktualnie OTWARTYCH backendów sled (slot `Open`). Inkrementowana
    /// przy `Closed→Open`, dekrementowana przy `Open→{Closed,Removed}`. Twardy
    /// rachunek otwartych baz — żadna ścieżka nie otwiera backendu bez
    /// inkrementacji, więc przeterminowany `Arc` nie może otworzyć bazy „obok"
    /// licznika (bug G).
    open_backends: AtomicU64,
    /// Override katalogu na dysku — produkcja używa `dirs::home_dir()`, testy
    /// wstrzykują tempdir (na `/mnt/e`), żeby nie śmiecić w `~`.
    root_override: Option<PathBuf>,
}

impl GraphManager {
    pub fn new(pool: DbPool) -> Self {
        Self {
            pool,
            collections: DashMap::new(),
            clock: AtomicU64::new(0),
            open_backends: AtomicU64::new(0),
            root_override: None,
        }
    }

    /// Konstruktor pinujący katalog danych pod `root` zamiast `~/.tentaflow`.
    /// Używany przez testy integracyjne i przyszłe CLI.
    pub fn with_root(pool: DbPool, root: PathBuf) -> Self {
        Self {
            pool,
            collections: DashMap::new(),
            clock: AtomicU64::new(0),
            open_backends: AtomicU64::new(0),
            root_override: Some(root),
        }
    }

    /// Następny znacznik logicznego zegara (monotoniczny, proces-szeroki).
    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }

    /// `<root>/<org>/<addon>/graph/<collection>.cozo` (override testowy) albo
    /// `<orgs_dir>/<org>/addons/<addon>/graph/<collection>.cozo` — root przez
    /// `paths::orgs_dir()` (respektuje `addons_data_dir` z Ustawien).
    fn file_path_for(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<PathBuf> {
        if let Some(root) = &self.root_override {
            Ok(root
                .join(org_id)
                .join(addon_id)
                .join("graph")
                .join(format!("{collection}.cozo")))
        } else {
            Ok(crate::paths::orgs_dir()
                .join(org_id)
                .join("addons")
                .join(addon_id)
                .join("graph")
                .join(format!("{collection}.cozo")))
        }
    }

    /// Deterministyczna ścieżka pliku kolekcji (`<root>/.../<collection>.cozo`).
    /// Udostępniona publicznie pod testy uninstall (sprawdzenie, że plik znika po
    /// sukcesie / zostaje przy błędzie kasowania). Nie otwiera bazy.
    pub fn collection_file_path(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
    ) -> Result<PathBuf> {
        self.file_path_for(org_id, addon_id, collection)
    }

    /// Eviction: dopóki w mapie jest więcej wpisów niż `MAX_OPEN_GRAPHS`, zamyka
    /// najdawniej używany (najmniejszy `last_used`). Zamknięcie jest REALNE i
    /// TERMINALNE dla wpisu (bug G): pod write-lockiem slotu ustawiamy `Removed`
    /// (drop backendu → sled flush+close, dekrement licznika otwartych), DOPIERO
    /// potem usuwamy wpis z mapy. Kolejność `mark_removed` PRZED `remove` jest
    /// kluczowa: przeterminowany `Arc<GraphEntry>` trzymany przez inny wątek,
    /// który czeka na write-lock, po zwolnieniu locka widzi `Removed` i re-fetchuje
    /// kanoniczny wpis zamiast otworzyć wyeksmitowaną bazę (brak double-open
    /// `WouldBlock`, brak przekroczenia `MAX_OPEN_GRAPHS`).
    fn evict_to_cap(&self) {
        while self.collections.len() > MAX_OPEN_GRAPHS {
            let victim = self
                .collections
                .iter()
                .min_by_key(|e| e.value().last_used.load(Ordering::Relaxed))
                .map(|e| e.key().clone());
            match victim {
                Some(k) => {
                    // Najpierw `mark_removed` (slot → Removed pod write-lockiem),
                    // DOPIERO potem zdejmij z mapy. Odwrotna kolejność otwierała
                    // okno: wątek z przeterminowanym Arc (slot Closed) brał
                    // write-lock i OTWIERAŁ backend między `remove` a `mark_removed`,
                    // a równoległy re-fetch interował drugi wpis na ten sam plik
                    // (double-open, over-cap). Z tą kolejnością przeterminowany Arc
                    // bierze slot-lock, widzi Removed i re-fetchuje kanoniczny wpis.
                    if let Some(entry) = self.collections.get(&k).map(|e| e.value().clone()) {
                        self.mark_removed(&entry);
                        self.collections.remove_if(&k, |_, v| Arc::ptr_eq(v, &entry));
                    }
                }
                None => break,
            }
        }
    }

    /// Przełącza slot wpisu w stan terminalny `Removed` pod write-lockiem, drop'ując
    /// żywy backend (sled flush+close) i dekrementując licznik otwartych. Wołane
    /// przez eviction i delete — po nim wpis nigdy nie otworzy bazy. Idempotentne.
    fn mark_removed(&self, entry: &Arc<GraphEntry>) {
        if let Ok(mut guard) = entry.slot.write() {
            if matches!(&*guard, BackendSlot::Open(_)) {
                self.open_backends.fetch_sub(1, Ordering::AcqRel);
            }
            *guard = BackendSlot::Removed;
        }
    }

    /// Atomowo get-or-insert wpis dla `key` w mapie BEZ otwierania backendu (slot
    /// `Closed`). Backend otwiera się leniwie w `with_read`/`with_write` pod
    /// write-lockiem wpisu, więc dwa równoległe dostępy do tej samej kolekcji
    /// dzielą JEDEN wpis i JEDEN open (dedup, bug #6). Bumpuje LRU i ewentualnie
    /// eksmituje nadmiar.
    fn intern_entry(
        &self,
        key: GraphKey,
        engine: GraphEngine,
        file_path: PathBuf,
    ) -> Arc<GraphEntry> {
        let now = self.tick();
        let entry = self
            .collections
            .entry(key)
            .or_insert_with(|| {
                Arc::new(GraphEntry {
                    slot: RwLock::new(BackendSlot::Closed),
                    engine,
                    file_path,
                    last_used: AtomicU64::new(now),
                })
            })
            .value()
            .clone();
        entry.last_used.store(now, Ordering::Relaxed);
        self.evict_to_cap();
        entry
    }

    /// Synchronizuje best-effort cache `node_count/edge_count` w SQLite z realnym
    /// stanem Cozo (źródło prawdy). Wywoływane przy otwarciu kolekcji.
    fn reconcile_counts(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        backend: &CozoBackend,
    ) {
        let (Ok(nodes), Ok(edges)) = (backend.node_count(), backend.edge_count()) else {
            return;
        };
        if let Ok(conn) = self.pool.write() {
            let _ = self.update_counts_locked(
                &conn,
                org_id,
                addon_id,
                collection,
                Some(nodes),
                Some(edges),
            );
        }
    }

    /// Interuje (lub pobiera) KANONICZNY wpis cache dla klucza BEZ dotykania DB.
    /// Ścieżka pliku jest DETERMINISTYCZNA z klucza (`file_path_for`), engine z
    /// wiersza DB jeśli istnieje, w p.p. domyślny dla buildu. Backend NIE jest tu
    /// otwierany, wiersz DB NIE jest tu tworzony — to dzieje się POD slot-write-
    /// lockiem w `with_write` (`ensure_row` + `open_backend`).
    ///
    /// Kluczowe dla bug cold-key create-vs-delete: punkt serializacji per-klucz
    /// (slot-lock kanonicznego wpisu) jest ustanawiany PRZED jakimkolwiek efektem
    /// ubocznym DB/plików. Delete bierze slot-lock TEGO SAMEGO kanonicznego wpisu
    /// (`canonical_entry_for`), więc cold-create i delete są wzajemnie wykluczające
    /// — nigdy nie powstaną żywe pliki/backend bez wiersza `addon_graph_collections`.
    fn entry_get_or_create(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
    ) -> Result<Arc<GraphEntry>> {
        validate_org_id(org_id).map_err(map_vector_err)?;
        validate_addon_id(addon_id).map_err(map_vector_err)?;
        validate_namespace_name(collection).map_err(map_vector_err)?;

        let key = GraphKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            collection: collection.to_string(),
        };
        if let Some(entry) = self.collections.get(&key) {
            entry.last_used.store(self.tick(), Ordering::Relaxed);
            return Ok(entry.value().clone());
        }

        // Engine z wiersza (metadane) jeśli istnieje — czysty odczyt, NIE tworzy
        // wiersza. Domyślny gdy wiersza brak; ścieżka zawsze deterministyczna.
        let engine = match self.load_engine(org_id, addon_id, collection)? {
            Some(engine_str) => GraphEngine::parse(&engine_str)
                .ok_or_else(|| GraphError::Db(format!("invalid engine '{engine_str}' in DB row")))?,
            None => GraphEngine::default_for_build(),
        };
        let file_path = self.file_path_for(org_id, addon_id, collection)?;
        Ok(self.intern_entry(key, engine, file_path))
    }

    /// Wstawia wiersz `addon_graph_collections` jeśli go nie ma (insert-if-missing),
    /// pod slot-write-lockiem kanonicznego wpisu — wszystkie efekty uboczne DB dla
    /// danego klucza dzieją się tu, wzajemnie wykluczone z delete (ten sam slot-
    /// lock). Idempotentny: istniejący wiersz → no-op (zachowuje liczniki ledgera).
    /// Quota kolekcji egzekwowana atomowo w `insert_row` (`BEGIN IMMEDIATE`).
    /// Wołane TYLKO trzymając write-lock slotu wpisu.
    fn ensure_row(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        engine: GraphEngine,
        file_path: &Path,
    ) -> Result<()> {
        if self.load_engine(org_id, addon_id, collection)?.is_some() {
            return Ok(());
        }
        match self.insert_row(org_id, addon_id, collection, engine, file_path) {
            Ok(()) => Ok(()),
            // Równoległy insert tej samej nowej kolekcji (bug #6): drugi wątek
            // dostaje UNIQUE-violation → wiersz już jest, traktuj jak sukces.
            // Quota-exceeded propagujemy normalnie.
            Err(GraphError::Db(_)) if self.load_engine(org_id, addon_id, collection)?.is_some() => {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Pobiera wpis cache BEZ tworzenia (ścieżka read). Błąd, gdy kolekcja nie
    /// istnieje w rejestrze. Backend otwierany leniwie w `with_*`.
    fn entry_get(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<Arc<GraphEntry>> {
        validate_org_id(org_id).map_err(map_vector_err)?;
        validate_addon_id(addon_id).map_err(map_vector_err)?;
        validate_namespace_name(collection).map_err(map_vector_err)?;

        let key = GraphKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            collection: collection.to_string(),
        };
        if let Some(entry) = self.collections.get(&key) {
            entry.last_used.store(self.tick(), Ordering::Relaxed);
            return Ok(entry.value().clone());
        }
        let Some(engine_str) = self.load_engine(org_id, addon_id, collection)? else {
            return Err(GraphError::CollectionNotFound {
                org_id: org_id.to_string(),
                addon_id: addon_id.to_string(),
                collection: collection.to_string(),
            });
        };
        let engine = GraphEngine::parse(&engine_str)
            .ok_or_else(|| GraphError::Db(format!("invalid engine '{engine_str}' in DB row")))?;
        // Ścieżka ZAWSZE deterministyczna z klucza (NIGDY z wiersza DB) — wiersz
        // `file_path` jest tylko informacyjny, nie źródło prawdy dla open.
        let file_path = self.file_path_for(org_id, addon_id, collection)?;
        Ok(self.intern_entry(key, engine, file_path))
    }

    /// Wykonuje `f` pod READ-lockiem backendu kolekcji (ścieżki query/neighbors/
    /// count/export). Pętla re-fetch (bug G): bierzemy kanoniczny wpis z mapy,
    /// próbujemy go otworzyć/użyć; jeśli wpis jest `Removed` (wyeksmitowany lub
    /// skasowany przez równoległy wątek), NIE otwieramy go — re-fetchujemy świeży
    /// wpis z mapy i ponawiamy. Bez tworzenia (ścieżka read): re-fetch nie znajdzie
    /// wiersza → `CollectionNotFound`.
    fn with_read<T>(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        mut f: impl FnMut(&CozoBackend) -> Result<T>,
    ) -> Result<T> {
        for attempt in 0..MAX_REFETCH_ATTEMPTS {
            let entry = self.entry_get(org_id, addon_id, collection)?;
            {
                let guard = entry
                    .slot
                    .read()
                    .map_err(|_| GraphError::Backend("graph entry lock poisoned".into()))?;
                match &*guard {
                    BackendSlot::Open(backend) => return f(backend),
                    BackendSlot::Removed => {} // re-fetch poniżej
                    BackendSlot::Closed => {
                        drop(guard);
                        match self.ensure_open(&entry, org_id, addon_id, collection)? {
                            OpenOutcome::Opened | OpenOutcome::AlreadyOpen => {
                                let guard = entry.slot.read().map_err(|_| {
                                    GraphError::Backend("graph entry lock poisoned".into())
                                })?;
                                // Otwarte → użyj; w p.p. stało się Removed → re-fetch poniżej.
                                if let BackendSlot::Open(backend) = &*guard {
                                    return f(backend);
                                }
                            }
                            OpenOutcome::Removed => {} // re-fetch poniżej
                        }
                    }
                }
            }
            // `Removed` zaobserwowany — odśwież wpis i spróbuj ponownie. Drobny
            // backoff przeciw livelockowi pod presją eviction (bug 3).
            self.collections
                .remove_if(&self.key_of(org_id, addon_id, collection), |_, v| {
                    matches!(&*v.slot.read().unwrap(), BackendSlot::Removed)
                });
            self.refetch_backoff(attempt);
        }
        // Wyczerpano próby — wymuś otwarcie kanonicznego wpisu BEZ eviction
        // (świadomy, chwilowy over-cap; gwarancja postępu zamiast starvation).
        let entry = self.entry_get(org_id, addon_id, collection)?;
        let guard = self.force_open(&entry, org_id, addon_id, collection, false)?;
        match &*guard {
            BackendSlot::Open(backend) => f(backend),
            _ => Err(GraphError::Backend("transient: open contention".into())),
        }
    }

    /// Wykonuje `f` pod WRITE-lockiem backendu kolekcji (ścieżki mutacji + quota).
    /// Lock obejmuje otwarcie (leniwe) ORAZ cały zakres `f`, więc per-kolekcyjne
    /// count+mutacja są atomowe wobec innych piszących i nie kolidują z
    /// delete/eviction (które też biorą write-lock). Pętla re-fetch (bug G):
    /// `Removed` → re-fetch kanonicznego wpisu z mapy (tworzy świeży, bo
    /// `create=true`) i ponów.
    fn with_write<T>(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        mut f: impl FnMut(&CozoBackend) -> Result<T>,
    ) -> Result<T> {
        for attempt in 0..MAX_REFETCH_ATTEMPTS {
            let entry = self.entry_get_or_create(org_id, addon_id, collection)?;
            {
                let mut guard = entry
                    .slot
                    .write()
                    .map_err(|_| GraphError::Backend("graph entry lock poisoned".into()))?;
                match &*guard {
                    BackendSlot::Open(backend) => return f(backend),
                    BackendSlot::Closed => {
                        // Efekty uboczne DB/plików dla cold-key dzieją się TU, pod
                        // slot-lockiem (wzajemnie wykluczone z delete tego samego
                        // wpisu): najpierw wiersz `addon_graph_collections`, dopiero
                        // potem open backendu. Bez tego cold-create wyścigał delete
                        // (żywe pliki/backend bez wiersza DB).
                        self.ensure_row(
                            org_id,
                            addon_id,
                            collection,
                            entry.engine,
                            &entry.file_path,
                        )?;
                        let backend = self.open_backend(&entry, org_id, addon_id, collection)?;
                        self.open_backends.fetch_add(1, Ordering::AcqRel);
                        *guard = BackendSlot::Open(backend);
                        match &*guard {
                            BackendSlot::Open(backend) => return f(backend),
                            _ => unreachable!("just set Open under the write lock"),
                        }
                    }
                    BackendSlot::Removed => {} // re-fetch poniżej
                }
            }
            self.collections
                .remove_if(&self.key_of(org_id, addon_id, collection), |_, v| {
                    matches!(&*v.slot.read().unwrap(), BackendSlot::Removed)
                });
            self.refetch_backoff(attempt);
        }
        // Wyczerpano próby — wymuś otwarcie kanonicznego wpisu BEZ eviction
        // (świadomy, chwilowy over-cap; gwarancja postępu zamiast starvation).
        let entry = self.entry_get_or_create(org_id, addon_id, collection)?;
        let guard = self.force_open(&entry, org_id, addon_id, collection, true)?;
        match &*guard {
            BackendSlot::Open(backend) => f(backend),
            _ => Err(GraphError::Backend("transient: open contention".into())),
        }
    }

    /// Krótki backoff w pętli re-fetch: pierwsze próby ustępują CPU (`yield_now`),
    /// dalsze śpią mikro-interwał, żeby przeterminowany wpis nie wracał w kółko pod
    /// presją eviction (bug 3, anty-livelock).
    fn refetch_backoff(&self, attempt: u32) {
        if attempt < 8 {
            std::thread::yield_now();
        } else {
            std::thread::sleep(std::time::Duration::from_micros(50));
        }
    }

    /// Wymusza otwarcie backendu kanonicznego wpisu pod write-lockiem BEZ eviction
    /// i zwraca trzymany write-guard ze slotem `Open` (chyba że wpis jest `Removed`
    /// — wtedy guard niesie `Removed`, a caller zwraca błąd przejściowy). Gwarancja
    /// postępu po wyczerpaniu pętli re-fetch: akceptujemy CHWILOWY over-cap zamiast
    /// livelocka. Świadomy transient over-cap — następny `intern_entry`/eviction
    /// ściągnie liczbę otwartych z powrotem do capu.
    ///
    /// `create_row=true` (ścieżka write) wymusza `ensure_row` POD slot-lockiem
    /// przed open — ta sama serializacja cold-key vs delete co w głównej pętli
    /// `with_write`. Read przekazuje `false` (NIE tworzy wiersza). Klucz jest
    /// zawsze potrzebny do rekonsyliacji liczników w `open_backend`.
    fn force_open<'a>(
        &self,
        entry: &'a Arc<GraphEntry>,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        create_row: bool,
    ) -> Result<std::sync::RwLockWriteGuard<'a, BackendSlot>> {
        let mut guard = entry
            .slot
            .write()
            .map_err(|_| GraphError::Backend("graph entry lock poisoned".into()))?;
        if let BackendSlot::Closed = &*guard {
            if create_row {
                self.ensure_row(org_id, addon_id, collection, entry.engine, &entry.file_path)?;
            }
            let backend = self.open_backend(entry, org_id, addon_id, collection)?;
            self.open_backends.fetch_add(1, Ordering::AcqRel);
            *guard = BackendSlot::Open(backend);
        }
        Ok(guard)
    }

    /// Leniwie otwiera backend wpisu pod write-lockiem (jeśli `Closed`). Dedup:
    /// pierwszy wątek otwiera, reszta widzi `Open`. `Removed` (eviction/delete w
    /// trakcie) → `OpenOutcome::Removed`, bez otwierania bazy (bug G).
    fn ensure_open(
        &self,
        entry: &Arc<GraphEntry>,
        org_id: &str,
        addon_id: &str,
        collection: &str,
    ) -> Result<OpenOutcome> {
        let mut guard = entry
            .slot
            .write()
            .map_err(|_| GraphError::Backend("graph entry lock poisoned".into()))?;
        match &*guard {
            BackendSlot::Open(_) => Ok(OpenOutcome::AlreadyOpen),
            BackendSlot::Removed => Ok(OpenOutcome::Removed),
            BackendSlot::Closed => {
                let backend = self.open_backend(entry, org_id, addon_id, collection)?;
                self.open_backends.fetch_add(1, Ordering::AcqRel);
                *guard = BackendSlot::Open(backend);
                Ok(OpenOutcome::Opened)
            }
        }
    }

    /// Otwiera `CozoBackend` z danych wpisu i rekonsyliuje liczniki rejestru z
    /// Cozo (źródło prawdy). Wołane TYLKO trzymając write-lock slotu wpisu.
    ///
    /// Klucz `(org, addon, collection)` jest PRZEKAZYWANY przez callera (który go
    /// zawsze ma), NIE odtwarzany skanem DashMap. Skan mapy pod slot-write-lockiem
    /// był źródłem zakleszczenia: writer trzymał slot-lock i czekał na read-lock
    /// sharda DashMap, podczas gdy równoległy delete (`canonical_entry_for` →
    /// `collections.entry`) trzymał write-lock tego samego sharda i czekał na ten
    /// slot-lock (AB-BA). Bez skanu mapy pod slot-lockiem cykl znika.
    fn open_backend(
        &self,
        entry: &Arc<GraphEntry>,
        org_id: &str,
        addon_id: &str,
        collection: &str,
    ) -> Result<CozoBackend> {
        let backend = CozoBackend::open_or_create(&entry.file_path, entry.engine)?;
        self.reconcile_counts(org_id, addon_id, collection, &backend);
        Ok(backend)
    }

    /// Klucz mapy z części składowych (helper pętli re-fetch).
    fn key_of(&self, org_id: &str, addon_id: &str, collection: &str) -> GraphKey {
        GraphKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            collection: collection.to_string(),
        }
    }

    /// Tworzy kolekcję (jeśli nie istnieje) i otwiera jej backend. Publiczny
    /// odpowiednik dawnego `get_or_create`, ale NIE zwraca uchwytu — tylko
    /// potwierdza istnienie. Używane przez ścieżki, które chcą zagwarantować
    /// utworzenie przed serią operacji.
    pub fn ensure_collection(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<()> {
        // Wiersz DB (`ensure_row`) i open backendu dzieją się POD slot-write-lockiem
        // wewnątrz `with_write` — ta sama serializacja cold-key vs delete co dla
        // upsertów. Quota kolekcji sprawdzana atomowo w `insert_row` (BEGIN IMMEDIATE).
        self.with_write(org_id, addon_id, collection, |_| Ok(()))
    }

    /// Czy kolekcja istnieje w rejestrze (bez otwierania backendu).
    pub fn collection_exists(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<bool> {
        validate_org_id(org_id).map_err(map_vector_err)?;
        validate_addon_id(addon_id).map_err(map_vector_err)?;
        validate_namespace_name(collection).map_err(map_vector_err)?;
        Ok(self.load_engine(org_id, addon_id, collection)?.is_some())
    }

    /// Liczba węzłów kolekcji (z Cozo, źródło prawdy). Read-lock.
    pub fn node_count(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<u64> {
        self.with_read(org_id, addon_id, collection, |b| b.node_count())
    }

    /// Liczba krawędzi kolekcji (z Cozo, źródło prawdy). Read-lock.
    pub fn edge_count(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<u64> {
        self.with_read(org_id, addon_id, collection, |b| b.edge_count())
    }

    /// Eksport CSR kolekcji (pod PPR w Rust). Otwiera kolekcję jeśli trzeba.
    /// Read-lock obejmuje cały eksport, więc CSR jest spójnym snapshotem.
    pub fn export_csr(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<Csr> {
        self.with_read(org_id, addon_id, collection, |b| b.export_edges())
    }

    /// Sąsiedzi węzła (out/in/both, opcjonalny filtr relacji, limit). Read-lock.
    #[allow(clippy::too_many_arguments)]
    pub fn neighbors(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        node: &str,
        direction: super::backend::NeighborDir,
        rel: Option<&str>,
        limit: u32,
    ) -> Result<Vec<(String, String, f64)>> {
        self.with_read(org_id, addon_id, collection, |b| {
            b.neighbors(node, direction, rel, limit)
        })
    }

    /// Globalny PageRank, top-N malejąco. Read-lock.
    ///
    /// Liczony w Rust nad CSR (`personalized_pagerank` z PUSTYMI seedami =
    /// jednostajna teleportacja = klasyczny globalny PageRank), bo wbudowany
    /// PageRank Cozo (`graph-algo`) ciągnie crate `graph_builder`, który
    /// konfliktuje z wersją rayon binarki (E0271/E0308 przy pełnym buildzie).
    /// To dokładnie ta sama semantyka co dawny cozo `<~ PageRank`, ale bez
    /// niezgodnej zależności.
    pub fn pagerank(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        top_n: u32,
        damping: f64,
        iterations: u32,
    ) -> Result<Vec<(String, f64)>> {
        let csr = self.export_csr(org_id, addon_id, collection)?;
        let mut scored =
            super::ppr::personalized_pagerank(&csr, &[], damping, iterations as usize);
        scored.truncate(top_n as usize);
        Ok(scored)
    }

    /// Personalized PageRank liczony w Rust nad CSR z Cozo (`ppr.rs`). Seedy to
    /// id węzłów stanowiących wektor personalizacji; nieznane id są pomijane.
    /// Zwraca top-N `(id, score)` malejąco. Read-lock obejmuje eksport CSR, więc
    /// PPR liczy się nad spójnym snapshotem grafu.
    ///
    /// SEMANTYKA SEEDÓW (retrieval z JAWNYMI kotwicami): ta ścieżka jest zawsze
    /// wołana z jawnie podaną listą seedów (host-fn `graph_ppr_v1`, `graph_search`
    /// op=ppr, GraphRAG). Rozróżniamy więc dwa przypadki:
    ///   * `seeds` PUSTE  — caller nie podał kotwic → globalny PageRank (jednostajna
    ///     teleportacja w `personalized_pagerank`); legalne wejście.
    ///   * `seeds` NIEPUSTE, ale ŻADEN nie istnieje w grafie (wszystkie odpadły po
    ///     filtrowaniu przez `id_index`) → PUSTY wynik. Personalized PageRank z
    ///     zerowymi kotwicami to brak wyniku, NIE globalny ranking — inaczej
    ///     zapytanie o encje spoza KG dostałoby top globalne encje (szum, łamie
    ///     degradację „brak encji → sam wektor").
    #[allow(clippy::too_many_arguments)]
    pub fn ppr(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        seeds: &[(String, f64)],
        top_n: u32,
        damping: f64,
        iterations: u32,
    ) -> Result<Vec<(String, f64)>> {
        let csr = self.export_csr(org_id, addon_id, collection)?;
        let index = csr.id_index();
        // Mapujemy `(id, waga)` -> `(idx, waga)`; nieznane id są pomijane. Wagi
        // niesie wektor personalizacji `personalized_pagerank` (P_init, R6).
        let seed_indices: Vec<(usize, f64)> = seeds
            .iter()
            .filter_map(|(id, w)| index.get(id.as_str()).map(|&idx| (idx, *w)))
            .collect();
        // Jawne seedy podane, ale żaden nie trafił w graf → brak ważnych kotwic.
        // Zwracamy pusto zamiast degenerować do globalnego PageRanku.
        if !seeds.is_empty() && seed_indices.is_empty() {
            return Ok(Vec::new());
        }
        let mut scored =
            super::ppr::personalized_pagerank(&csr, &seed_indices, damping, iterations as usize);
        scored.truncate(top_n as usize);
        Ok(scored)
    }

    /// PPR z pełnym sygnałem P_init structure-aware (MemGraphRAG §6.2) liczonym
    /// nad JEDNYM snapshotem CSR. To ścieżka GraphRAG retrievalu: kara log-degree,
    /// cap liczby kotwic i sam PPR MUSZĄ widzieć ten sam graf, więc eksportujemy
    /// CSR dokładnie raz (inaczej stopnie i ranking byłyby z różnych snapshotów,
    /// a kotwica capnięta przed przeważeniem nigdy nie zostałaby rozważona).
    ///
    /// `seeds` to wagi BAZOWE (`base × relevance`, jeszcze NIE capnięte) — caller
    /// (adapter RAG) dorzuca boost relevance, bo zależy on od pasaży wektorowych.
    /// Tutaj domykamy P_init dwoma krokami nad tym samym CSR:
    ///   1. FILTR ZNANYCH: mapujemy kandydatów na indeksy CSR i ODRZUCAMY nieznane
    ///      ZANIM cokolwiek capniemy. Wysokowagowy seed spoza grafu nie może wyprzeć
    ///      znanej kotwicy z cap-u — inaczej PPR dostałby pusty/zubożony wektor mimo
    ///      obecnych znanych kotwic.
    ///   2. KARA LOG-DEGREE: `w /= 1 + ln(1 + degree)` na ZNANYCH kotwicach z tego
    ///      CSR (węzeł-hub jest słabą, mało selektywną kotwicą).
    ///   3. CAP PO PRZEWAŻENIU: sortujemy ZNANE kotwice po wadze FINALNEJ (malejąco)
    ///      i ucinamy do `max_seeds`. Kotwica z wysoką wagą po log-degree/relevance,
    ///      ale leksykalnie poza pierwszymi `max_seeds`, dzięki temu JEST rozważona.
    ///
    /// Semantyka pustych/nieznanych kotwic jak w [`Self::ppr`]: jawne, ale w całości
    /// nieznane seedy → pusty wynik (nie degenerujemy do globalnego PageRanku).
    #[allow(clippy::too_many_arguments)]
    pub fn ppr_with_p_init(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        seeds: &[(String, f64)],
        max_seeds: usize,
        top_n: u32,
        damping: f64,
        iterations: u32,
    ) -> Result<Vec<(String, f64)>> {
        let csr = self.export_csr(org_id, addon_id, collection)?;
        let index = csr.id_index();
        let degrees = csr.total_degrees();

        // Krok 1: mapuj na indeksy TEGO CSR i ODFILTRUJ nieznane PRZED capem —
        // nieznana, wysokowagowa kotwica nie może wyprzeć znanej z max_seeds.
        let mut weighted: Vec<(usize, f64)> = seeds
            .iter()
            .filter_map(|(id, w)| index.get(id.as_str()).map(|&idx| (idx, *w)))
            .collect();
        // Jawne kotwice podane, ale żadna nie trafiła w graf → pusto (degradacja).
        if !seeds.is_empty() && weighted.is_empty() {
            return Ok(Vec::new());
        }

        // Krok 2: kara log-degree na ZNANYCH kotwicach z tego snapshotu.
        for (idx, w) in &mut weighted {
            *w /= 1.0 + (1.0 + degrees[*idx] as f64).ln();
        }

        // Krok 3: cap PO przeważeniu — sort po wadze finalnej, utnij do max_seeds.
        // Cap dotyczy WYŁĄCZNIE znanych kotwic, więc nieznane nie zajmują slotów.
        weighted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        weighted.truncate(max_seeds);

        let seed_indices = weighted;
        let mut scored =
            super::ppr::personalized_pagerank(&csr, &seed_indices, damping, iterations as usize);
        scored.truncate(top_n as usize);
        Ok(scored)
    }

    /// Soft-delete (tombstone) węzła `id` + wykluczenie jego krawędzi z retrievalu.
    /// Wiersz węzła ZOSTAJE (O(1) `:put` markera), więc liczba węzłów i ledger
    /// quoty się NIE zmieniają — fizyczny purge to późniejsza kompakcja. Krawędzie
    /// incydentne są pomijane przez retrieval (join z nie-tombstone węzłami), nie
    /// kasowane fizycznie. Write-lock. Zwraca `(removed, node_count, edge_count)`.
    pub fn delete_node_in(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        id: &str,
    ) -> Result<(bool, u64, u64)> {
        self.with_write(org_id, addon_id, collection, |backend| {
            let removed = backend.delete_node(id)?;
            let nodes = backend.node_count()?;
            let edges = backend.edge_count()?;
            Ok((removed, nodes, edges))
        })
    }

    /// Soft-delete pojedynczej krawędzi `(src, rel, dst)` (`alive=false`, O(1)).
    /// Wiersz zostaje (ledger quoty bez zmian); retrieval pomija. Write-lock.
    pub fn delete_edge_in(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        src: &str,
        rel: &str,
        dst: &str,
    ) -> Result<(bool, u64, u64)> {
        self.with_write(org_id, addon_id, collection, |backend| {
            let removed = backend.delete_edge(src, rel, dst)?;
            let nodes = backend.node_count()?;
            let edges = backend.edge_count()?;
            Ok((removed, nodes, edges))
        })
    }

    /// Alias soft-delete węzła dla wariantu `GraphDeleteTarget::Tombstone` — ta
    /// sama semantyka co `delete_node_in` (delete węzła w Etapie 0 = tombstone).
    pub fn tombstone_node_in(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        id: &str,
    ) -> Result<(bool, u64, u64)> {
        self.delete_node_in(org_id, addon_id, collection, id)
    }

    /// Upsert węzła z egzekwowaniem GLOBALNEJ quoty węzłów na (org, addon),
    /// atomowej także MIĘDZY kolekcjami (bug F). Protokół: pod write-lockiem
    /// kolekcji ustalamy `is_new = !node_exists(id)` (replace istniejącego id nie
    /// zmienia sumy, więc nie rezerwuje quoty). Dla nowego id rezerwujemy 1
    /// jednostkę w atomowym ledgerze SQLite (`reserve_node_quota`: `BEGIN
    /// IMMEDIATE` → `SELECT SUM(node_count) WHERE org,addon` → jeśli `+1 > limit`
    /// reject → `UPDATE node_count+=1 WHERE collection` → COMMIT). Globalny writer
    /// SQLite serializuje to między WSZYSTKIMI kolekcjami addona — dwóch piszących
    /// do różnych kolekcji konkuruje o ten sam SUM, więc razem nie przekroczą
    /// limitu. Potem mutacja Cozo; gdy padnie → kompensata `node_count-=1`
    /// (zwolnienie rezerwacji), błąd propagowany. Sprawdzenie istnienia id
    /// parametryzowane (`$id`), nie `format!()`.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_node_with_quota(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        id: &str,
        label: &str,
        props_json: &str,
        provenance_json: &str,
    ) -> Result<u64> {
        let max_nodes = self.resolve_node_limit(addon_id);

        self.with_write(org_id, addon_id, collection, |backend| {
            let is_new = !backend.node_exists(id)?;
            if is_new {
                // Rezerwacja w atomowym ledgerze PRZED mutacją Cozo.
                self.reserve_node_quota(org_id, addon_id, collection, max_nodes)?;
                // Mutacja grafu; przy błędzie zwolnij rezerwację.
                if let Err(e) = backend.upsert_node(id, label, props_json, provenance_json) {
                    self.release_node_quota(org_id, addon_id, collection);
                    return Err(e);
                }
            } else {
                backend.upsert_node(id, label, props_json, provenance_json)?;
            }
            backend.node_count()
        })
    }

    /// Upsert krawędzi z egzekwowaniem GLOBALNEJ quoty krawędzi na (org, addon),
    /// atomowej między kolekcjami (bug F). Symetryczny do
    /// `upsert_node_with_quota`: rezerwacja w atomowym ledgerze → mutacja Cozo →
    /// kompensata przy błędzie.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_edge_with_quota(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        src: &str,
        rel: &str,
        dst: &str,
        weight: f64,
        props_json: &str,
        provenance_json: &str,
    ) -> Result<u64> {
        let max_edges = self.resolve_edge_limit(addon_id);

        self.with_write(org_id, addon_id, collection, |backend| {
            let is_new = !backend.edge_exists(src, rel, dst)?;
            if is_new {
                self.reserve_edge_quota(org_id, addon_id, collection, max_edges)?;
                if let Err(e) =
                    backend.upsert_edge(src, rel, dst, weight, props_json, provenance_json)
                {
                    self.release_edge_quota(org_id, addon_id, collection);
                    return Err(e);
                }
            } else {
                backend.upsert_edge(src, rel, dst, weight, props_json, provenance_json)?;
            }
            backend.edge_count()
        })
    }

    /// Atomowa rezerwacja 1 węzła w globalnym ledgerze quoty (bug F). W jednej
    /// `BEGIN IMMEDIATE`: liczy sumę `node_count` po WSZYSTKICH kolekcjach
    /// (org, addon); gdy `suma + 1 > max` zwraca `NodeQuotaExceeded` (rollback),
    /// w p.p. inkrementuje `node_count` bieżącej kolekcji o 1 i commituje.
    /// Globalny writer SQLite czyni to wzajemnie wykluczającym między kolekcjami.
    fn reserve_node_quota(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        max_nodes: u64,
    ) -> Result<()> {
        let conn = self
            .pool
            .write()
            .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| GraphError::Db(e.to_string()))?;

        let total: i64 = match conn.query_row(
            "SELECT COALESCE(SUM(node_count), 0) FROM addon_graph_collections \
             WHERE org_id = ?1 AND addon_id = ?2",
            rusqlite::params![org_id, addon_id],
            |r| r.get(0),
        ) {
            Ok(t) => t,
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                return Err(GraphError::Db(e.to_string()));
            }
        };
        let current = total.max(0) as u64;
        if current.saturating_add(1) > max_nodes {
            let _ = conn.execute("ROLLBACK", []);
            return Err(GraphError::NodeQuotaExceeded {
                addon_id: addon_id.to_string(),
                current,
                max: max_nodes,
            });
        }
        if let Err(e) = conn.execute(
            "UPDATE addon_graph_collections SET node_count = node_count + 1 \
             WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
            rusqlite::params![org_id, addon_id, collection],
        ) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(GraphError::Db(e.to_string()));
        }
        conn.execute("COMMIT", [])
            .map_err(|e| GraphError::Db(e.to_string()))?;
        Ok(())
    }

    /// Zwalnia rezerwację 1 węzła (kompensata, gdy mutacja Cozo padła). Najlepszy
    /// wysiłek — `reconcile_counts` przy otwarciu i tak skoryguje dryf z Cozo.
    fn release_node_quota(&self, org_id: &str, addon_id: &str, collection: &str) {
        if let Ok(conn) = self.pool.write() {
            let _ = conn.execute(
                "UPDATE addon_graph_collections \
                 SET node_count = MAX(node_count - 1, 0) \
                 WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
                rusqlite::params![org_id, addon_id, collection],
            );
        }
    }

    /// Atomowa rezerwacja 1 krawędzi w globalnym ledgerze quoty (bug F).
    fn reserve_edge_quota(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        max_edges: u64,
    ) -> Result<()> {
        let conn = self
            .pool
            .write()
            .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| GraphError::Db(e.to_string()))?;

        let total: i64 = match conn.query_row(
            "SELECT COALESCE(SUM(edge_count), 0) FROM addon_graph_collections \
             WHERE org_id = ?1 AND addon_id = ?2",
            rusqlite::params![org_id, addon_id],
            |r| r.get(0),
        ) {
            Ok(t) => t,
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                return Err(GraphError::Db(e.to_string()));
            }
        };
        let current = total.max(0) as u64;
        if current.saturating_add(1) > max_edges {
            let _ = conn.execute("ROLLBACK", []);
            return Err(GraphError::EdgeQuotaExceeded {
                addon_id: addon_id.to_string(),
                current,
                max: max_edges,
            });
        }
        if let Err(e) = conn.execute(
            "UPDATE addon_graph_collections SET edge_count = edge_count + 1 \
             WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
            rusqlite::params![org_id, addon_id, collection],
        ) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(GraphError::Db(e.to_string()));
        }
        conn.execute("COMMIT", [])
            .map_err(|e| GraphError::Db(e.to_string()))?;
        Ok(())
    }

    /// Zwalnia rezerwację 1 krawędzi (kompensata przy błędzie Cozo).
    fn release_edge_quota(&self, org_id: &str, addon_id: &str, collection: &str) {
        if let Ok(conn) = self.pool.write() {
            let _ = conn.execute(
                "UPDATE addon_graph_collections \
                 SET edge_count = MAX(edge_count - 1, 0) \
                 WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
                rusqlite::params![org_id, addon_id, collection],
            );
        }
    }

    fn update_counts_locked(
        &self,
        conn: &rusqlite::Connection,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        node_count: impl Into<Option<u64>>,
        edge_count: impl Into<Option<u64>>,
    ) -> Result<()> {
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let node_count = node_count.into();
        let edge_count = edge_count.into();
        match (node_count, edge_count) {
            (Some(n), Some(e)) => conn.execute(
                "UPDATE addon_graph_collections SET node_count = ?1, edge_count = ?2, updated_at = ?3 \
                 WHERE org_id = ?4 AND addon_id = ?5 AND collection = ?6",
                rusqlite::params![n as i64, e as i64, now, org_id, addon_id, collection],
            ),
            (Some(n), None) => conn.execute(
                "UPDATE addon_graph_collections SET node_count = ?1, updated_at = ?2 \
                 WHERE org_id = ?3 AND addon_id = ?4 AND collection = ?5",
                rusqlite::params![n as i64, now, org_id, addon_id, collection],
            ),
            (None, Some(e)) => conn.execute(
                "UPDATE addon_graph_collections SET edge_count = ?1, updated_at = ?2 \
                 WHERE org_id = ?3 AND addon_id = ?4 AND collection = ?5",
                rusqlite::params![e as i64, now, org_id, addon_id, collection],
            ),
            (None, None) => return Ok(()),
        }
        .map_err(|e| GraphError::Db(e.to_string()))?;
        Ok(())
    }

    /// Limit węzłów: kolumna `graph_nodes_max` (>0) ma pierwszeństwo, w p.p.
    /// twarda stała. Bierze własny read-lock (poza per-kolekcyjnym write-lockiem).
    fn resolve_node_limit(&self, addon_id: &str) -> u64 {
        let Ok(conn) = self.pool.read() else {
            return MAX_NODES_PER_ADDON;
        };
        let v: i64 = conn
            .query_row(
                "SELECT graph_nodes_max FROM addon_resource_limits WHERE addon_id = ?1",
                rusqlite::params![addon_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if v > 0 {
            v as u64
        } else {
            MAX_NODES_PER_ADDON
        }
    }

    fn resolve_edge_limit(&self, addon_id: &str) -> u64 {
        let Ok(conn) = self.pool.read() else {
            return MAX_EDGES_PER_ADDON;
        };
        let v: i64 = conn
            .query_row(
                "SELECT graph_edges_max FROM addon_resource_limits WHERE addon_id = ?1",
                rusqlite::params![addon_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if v > 0 {
            v as u64
        } else {
            MAX_EDGES_PER_ADDON
        }
    }

    /// Sprawdza limit kolekcji na (org, addon) przed utworzeniem nowej.
    /// Atomowość samego sprawdzenia+insertu zapewnia `insert_row`
    /// (PK `(org_id, addon_id, collection)` + transakcja `BEGIN IMMEDIATE`).
    pub fn check_collection_quota(&self, org_id: &str, addon_id: &str) -> Result<()> {
        let conn = self
            .pool
            .read()
            .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM addon_graph_collections \
                 WHERE org_id = ?1 AND addon_id = ?2",
                rusqlite::params![org_id, addon_id],
                |r| r.get(0),
            )
            .map_err(|e| GraphError::Db(e.to_string()))?;
        if count as u32 >= MAX_COLLECTIONS_PER_ADDON {
            return Err(GraphError::CollectionQuotaExceeded {
                addon_id: addon_id.to_string(),
                current: count as u32,
                max: MAX_COLLECTIONS_PER_ADDON,
            });
        }
        Ok(())
    }

    /// Kasuje jedną kolekcję. Protokół (bug H — serializacja per-klucz nawet przy
    /// cache-miss): WSZYSTKO dzieje się pod write-lockiem slotu KANONICZNEGO wpisu
    /// (interujemy go, jeśli go nie ma — patrz `seal_key_for_delete`). Pod tym
    /// lockiem kolejno (files-before-row): zamknięcie backendu i oznaczenie slotu
    /// `Removed`, potem skasowanie PLIKÓW `.cozo`, dopiero na końcu `DELETE FROM
    /// addon_graph_collections` i zdjęcie wpisu z mapy. Wiersz znika DOPIERO po
    /// udanym usunięciu plików, więc błąd I/O przerywa przed `DELETE` (wiersz +
    /// pliki zostają, retry możliwy) i NIGDY nie powstają osierocone pliki bez
    /// wiersza. Slot `Removed` pod lockiem sprawia, że równoległy `get_or_create`
    /// po sukcesie (brak wiersza) tworzy ŚWIEŻĄ pustą kolekcję zamiast wskrzeszać
    /// stare pliki, a po porażce reotwiera tę samą bazę z zachowanych plików.
    /// Kasowanie pliku jest atomowe względem każdej innej operacji na tym kluczu,
    /// więc nigdy nie biegnie równolegle z `sled::open` (brak korupcji).
    /// Idempotentne (brak wiersza / pliku => OK).
    pub fn delete_collection(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<()> {
        validate_org_id(org_id).map_err(map_vector_err)?;
        validate_addon_id(addon_id).map_err(map_vector_err)?;
        validate_namespace_name(collection).map_err(map_vector_err)?;

        let key = GraphKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            collection: collection.to_string(),
        };
        self.seal_key_for_delete(&key)
    }

    /// Kanoniczny wpis dla `key` BEZ otwierania backendu i BEZ eviction (delete
    /// zaraz go usunie). Interuje wpis, jeśli go nie ma — to gwarantuje, że delete
    /// i równoległy `get_or_create` dzielą TEN SAM `Arc<GraphEntry>` i serializują
    /// się na jego write-locku (bug H przy cache-miss).
    fn canonical_entry_for(&self, key: &GraphKey, engine: GraphEngine, file_path: PathBuf) -> Arc<GraphEntry> {
        self.collections
            .entry(key.clone())
            .or_insert_with(|| {
                Arc::new(GraphEntry {
                    slot: RwLock::new(BackendSlot::Closed),
                    engine,
                    file_path,
                    last_used: AtomicU64::new(self.tick()),
                })
            })
            .value()
            .clone()
    }

    /// Pełny protokół kasowania pod write-lockiem slotu kanonicznego wpisu (bug H).
    /// Ścieżka pliku jest DETERMINISTYCZNA z `GraphKey` (`file_path_for`), więc
    /// liczymy ją z klucza niezależnie od tego, czy wiersz DB istnieje — wiersz DB
    /// służy tylko do metadanych (engine), NIGDY do lokalizacji pliku.
    ///
    /// KOLEJNOŚĆ (spójność przy awarii): close-handle → skasuj PLIKI → dopiero
    /// potem skasuj WIERSZ rejestru → zdejmij wpis z mapy. Wiersz znika DOPIERO po
    /// udanym usunięciu plików, więc błąd I/O kasowania PRZERYWA przed `DELETE` —
    /// wiersz zostaje, operacja jest retry-able i NIGDY nie powstają osierocone
    /// pliki bez wiersza (orphan-files). Slot ustawiamy na `Removed` pod lockiem,
    /// więc równoległy `get_or_create` czekający na ten lock re-fetchuje: po sukcesie
    /// (brak wiersza) tworzy świeżą pustą kolekcję, po porażce (wiersz + pliki nadal
    /// są) reotwiera tę samą bazę — stan retry pozostaje spójny.
    fn seal_key_for_delete(&self, key: &GraphKey) -> Result<()> {
        // Ścieżka deterministyczna z klucza — ta sama, której użył writer przy
        // tworzeniu, więc współbieżny writer i delete operują na tym samym pliku
        // pod tym samym slot-lockiem (serializacja, brak otwierania pustej ścieżki).
        let path = self.file_path_for(&key.org_id, &key.addon_id, &key.collection)?;

        // Engine z DB (metadane). Domyślny, gdy wiersza nie ma — wpis i tak będzie
        // `Removed`, nigdy nie otworzy bazy.
        let engine: GraphEngine = {
            let conn = self
                .pool
                .read()
                .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
            conn.query_row(
                "SELECT engine FROM addon_graph_collections \
                 WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
                rusqlite::params![key.org_id, key.addon_id, key.collection],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|eng| GraphEngine::parse(&eng))
            .unwrap_or_else(GraphEngine::default_for_build)
        };

        let entry = self.canonical_entry_for(key, engine, path.clone());

        let mut guard = entry
            .slot
            .write()
            .map_err(|_| GraphError::Backend("graph entry lock poisoned".into()))?;

        // 1) Zamknij backend (dekrement licznika, gdy był otwarty) i oznacz slot
        //    `Removed` — pod lockiem żaden inny wątek nie operuje na tej bazie.
        if matches!(&*guard, BackendSlot::Open(_)) {
            self.open_backends.fetch_sub(1, Ordering::AcqRel);
        }
        *guard = BackendSlot::Removed;

        // 2) Skasuj PLIKI. Błąd → PRZERWIJ przed `DELETE` wiersza: wiersz zostaje,
        //    pliki zostają, operacja jest retry-able, brak orphan-files bez wiersza.
        if let Err(e) = remove_cozo_files(&path) {
            // Wpis zostaje w mapie z `Removed`; następny dostęp re-fetchnie wiersz
            // (nadal istnieje) i reotworzy bazę z tych samych plików.
            drop(guard);
            return Err(e);
        }

        // 3) Pliki skasowane → dopiero teraz skasuj WIERSZ rejestru.
        {
            let conn = self
                .pool
                .write()
                .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
            conn.execute(
                "DELETE FROM addon_graph_collections \
                 WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
                rusqlite::params![key.org_id, key.addon_id, key.collection],
            )
            .map_err(|e| GraphError::Db(e.to_string()))?;
        }
        drop(guard);

        // 4) Zdejmij kanoniczny wpis tylko jeśli to NADAL ten sam Arc (równoległy
        //    get_or_create mógł go już zastąpić świeżym po naszym `Removed`).
        self.collections
            .remove_if(key, |_, v| Arc::ptr_eq(v, &entry));

        Ok(())
    }

    /// Kasuje WSZYSTKIE kolekcje grafowe addona W DANEJ ORGANIZACJI: kluczowane
    /// `(org_id, addon_id)`, NIGDY samym `addon_id` — inny tenant z tym samym
    /// `addon_id` pozostaje nietknięty. Zamknięcie backendów -> wiersze DB ->
    /// pliki. Wpinane w `uninstall` w slice B2.
    pub fn delete_all_for_addon(&self, org_id: &str, addon_id: &str) -> Result<()> {
        validate_org_id(org_id).map_err(map_vector_err)?;
        validate_addon_id(addon_id).map_err(map_vector_err)?;

        let collections: Vec<String> = {
            let conn = self
                .pool
                .read()
                .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
            // Brak tabeli rejestru == addon nie ma żadnych kolekcji grafowych →
            // nie ma czego sprzątać. Ten przypadek zachodzi na ścieżkach instalacji,
            // które nigdy nie utworzyły schematu grafu (np. minimalne DB w testach
            // jednostkowych). Tylko `no such table` traktujemy jako pustą listę;
            // każdy inny błąd DB propagujemy (nie maskujemy korupcji).
            if !table_exists(&conn, "addon_graph_collections")? {
                return Ok(());
            }
            let mut stmt = conn
                .prepare(
                    "SELECT collection FROM addon_graph_collections \
                     WHERE org_id = ?1 AND addon_id = ?2",
                )
                .map_err(|e| GraphError::Db(e.to_string()))?;
            let mapped = stmt
                .query_map(rusqlite::params![org_id, addon_id], |r| {
                    r.get::<_, String>(0)
                })
                .map_err(|e| GraphError::Db(e.to_string()))?;
            mapped
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| GraphError::Db(e.to_string()))?
        };

        // Każdą kolekcję kasujemy tym samym serializowanym per-klucz protokołem co
        // `delete_collection` (bug H) — wiersz DB + pliki pod slot-write-lockiem.
        let mut first_err: Option<GraphError> = None;
        for collection in &collections {
            let key = GraphKey {
                org_id: org_id.to_string(),
                addon_id: addon_id.to_string(),
                collection: collection.clone(),
            };
            if let Err(e) = self.seal_key_for_delete(&key) {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Usuwa z cache wszystkie otwarte backendy addona (po WSZYSTKICH org) BEZ
    /// kasowania danych na dysku — następny dostęp odbuduje backend ze świeżego
    /// wpisu. Slot przechodzi w `Removed` (terminalny) pod write-lockiem, więc
    /// żaden przeterminowany `Arc` nie reotworzy tej samej bazy „obok" świeżego
    /// wpisu (bug G); dane na dysku zostają, więc re-fetch reotwiera ten sam plik.
    /// Wpinane w `materialize_addon_derived_state` (slice B2).
    pub fn invalidate_addon(&self, addon_id: &str) {
        let entries: Vec<(GraphKey, Arc<GraphEntry>)> = self
            .collections
            .iter()
            .filter(|e| e.key().addon_id == addon_id)
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        for (key, entry) in entries {
            // Najpierw `mark_removed` (slot → Removed pod write-lockiem), DOPIERO
            // potem zdejmij z mapy. Odwrotna kolejność otwierała okno na re-open
            // przeterminowanego Arc „obok" świeżego wpisu (double-open na ten sam
            // plik). Pod tą kolejnością stale-Arc widzi Removed i re-fetchuje.
            self.mark_removed(&entry);
            self.collections
                .remove_if(&key, |_, v| Arc::ptr_eq(v, &entry));
        }
    }

    /// Zamyka WSZYSTKIE otwarte backendy (migracja katalogu danych addonów —
    /// sled trzyma otwarte pliki, które muszą zostać zwolnione przed
    /// przeniesieniem katalogu). Ta sama kolejność `mark_removed` → `remove_if`
    /// co w `invalidate_addon`; dane na dysku zostają, następny dostęp otwiera
    /// plik z nowej lokalizacji (`file_path_for` liczy ścieżkę na żądanie).
    pub fn invalidate_all(&self) {
        let entries: Vec<(GraphKey, Arc<GraphEntry>)> = self
            .collections
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        for (key, entry) in entries {
            self.mark_removed(&entry);
            self.collections
                .remove_if(&key, |_, v| Arc::ptr_eq(v, &entry));
        }
    }

    /// Akcesor testowy do współdzielonej puli — pozwala testom asertować na
    /// wierszach `addon_graph_collections` przez ten sam (in-memory) connection.
    #[cfg(test)]
    pub(crate) fn pool(&self) -> &DbPool {
        &self.pool
    }

    /// Liczba aktualnie otwartych backendów (slot `Open`). Test eviction:
    /// liczy realnie żywe backendy sled, nie wpisy z leniwie zamkniętym slotem.
    #[cfg(test)]
    pub(crate) fn open_handles(&self) -> usize {
        self.collections
            .iter()
            .filter(|e| {
                e.value()
                    .slot
                    .read()
                    .map(|g| matches!(&*g, BackendSlot::Open(_)))
                    .unwrap_or(false)
            })
            .count()
    }

    /// Liczba wpisów w mapie (otwarte + leniwie zamknięte). Cap eviction działa
    /// na tej liczbie; open backendy są jej podzbiorem.
    #[cfg(test)]
    pub(crate) fn cached_entries(&self) -> usize {
        self.collections.len()
    }

    /// Wartość licznika `open_backends` (rachunek otwartych baz sled prowadzony
    /// przy open/close/evict/delete). Test bug G: licznik nie może przekroczyć
    /// capu ani się rozjechać ze stanem slotów pod obciążeniem.
    #[cfg(test)]
    pub(crate) fn open_backends_counter(&self) -> u64 {
        self.open_backends.load(Ordering::Acquire)
    }

    /// Czyta kolumnę `engine` wiersza kolekcji (metadane). `file_path` wiersza
    /// jest tylko informacyjny — ścieżka pliku liczona deterministycznie z klucza
    /// (`file_path_for`), więc tu jej NIE czytamy. `None` gdy wiersza brak.
    fn load_engine(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
    ) -> Result<Option<String>> {
        let conn = self
            .pool
            .read()
            .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
        let engine = conn
            .query_row(
                "SELECT engine FROM addon_graph_collections \
                 WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
                rusqlite::params![org_id, addon_id, collection],
                |r| r.get::<_, String>(0),
            )
            .ok();
        Ok(engine)
    }

    /// Wstawia wiersz kolekcji atomowo wobec quoty: `BEGIN IMMEDIATE` bierze
    /// write-lock SQLite na czas (count kolekcji + INSERT), więc dwa równoległe
    /// `get_or_create` na progu `MAX_COLLECTIONS_PER_ADDON` nie wstawią obu.
    /// Konflikt PK (wyścig o tę samą collection) także odpada — drugi INSERT
    /// failuje, transakcja jest rollbackowana, a `entry_get_or_create` ładuje
    /// istniejący wiersz (bug #6).
    fn insert_row(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        engine: GraphEngine,
        file_path: &Path,
    ) -> Result<()> {
        let conn = self
            .pool
            .write()
            .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| GraphError::Db(e.to_string()))?;

        let max = MAX_COLLECTIONS_PER_ADDON as i64;
        let count: i64 = match conn.query_row(
            "SELECT COUNT(*) FROM addon_graph_collections WHERE org_id = ?1 AND addon_id = ?2",
            rusqlite::params![org_id, addon_id],
            |r| r.get(0),
        ) {
            Ok(c) => c,
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                return Err(GraphError::Db(e.to_string()));
            }
        };
        if count >= max {
            let _ = conn.execute("ROLLBACK", []);
            return Err(GraphError::CollectionQuotaExceeded {
                addon_id: addon_id.to_string(),
                current: count as u32,
                max: MAX_COLLECTIONS_PER_ADDON,
            });
        }

        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        if let Err(e) = conn.execute(
            "INSERT INTO addon_graph_collections \
             (org_id, addon_id, collection, file_path, engine, node_count, edge_count, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6, ?6)",
            rusqlite::params![
                org_id,
                addon_id,
                collection,
                file_path.to_string_lossy().to_string(),
                engine.as_str(),
                now,
            ],
        ) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(GraphError::Db(e.to_string()));
        }
        conn.execute("COMMIT", [])
            .map_err(|e| GraphError::Db(e.to_string()))?;
        Ok(())
    }
}

/// Mapuje błąd walidacji z warstwy vector na `GraphError::InvalidCollectionName`.
/// Walidatory nazw są współdzielone (`validate_org_id` upubliczniony), więc ich
/// `VectorError::InvalidNamespaceName` tłumaczymy na odpowiednik grafowy zamiast
/// przeciekać typ vector.
fn map_vector_err(e: crate::services::vector::VectorError) -> GraphError {
    match e {
        crate::services::vector::VectorError::InvalidNamespaceName(name) => {
            GraphError::InvalidCollectionName(name)
        }
        other => GraphError::Backend(other.to_string()),
    }
}

/// Czy tabela istnieje w bieżącej bazie. Pozwala `delete_all_for_addon` tolerować
/// brak rejestru grafu (DB instalacji, które nigdy nie utworzyły schematu grafu)
/// bez maskowania innych błędów DB sztywnym dopasowaniem łańcucha błędu.
fn table_exists(conn: &rusqlite::Connection, table: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            rusqlite::params![table],
            |r| r.get(0),
        )
        .map_err(|e| GraphError::Db(e.to_string()))?;
    Ok(count > 0)
}

/// Usuwa plik kolekcji Cozo wraz z plikami pomocniczymi SQLite (`-wal`/`-shm`).
/// Idempotentne: brak pliku => OK. Toleruje Windows (plik chwilowo trzymany przez
/// zamykający się uchwyt): krótki retry zanim zwróci błąd I/O.
fn remove_cozo_files(path: &Path) -> Result<()> {
    let candidates = [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.to_string_lossy())),
        PathBuf::from(format!("{}-shm", path.to_string_lossy())),
    ];
    for p in candidates {
        if !p.exists() {
            continue;
        }
        let mut last_err = None;
        for attempt in 0..5 {
            let res = if p.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
            match res {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt < 4 {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
            }
        }
        if let Some(e) = last_err {
            // Plik mógł zniknąć między pętlami (inny wątek) — wtedy OK.
            if p.exists() {
                return Err(GraphError::Io {
                    path: Some(p),
                    source: e,
                });
            }
        }
    }
    Ok(())
}
