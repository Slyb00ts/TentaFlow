// =============================================================================
// Plik: sync/core_baseline.rs
// Opis: Rdzen baseline-adopt pairingu — deterministyczna elekcja dawcy,
//       snapshot platformowych tabel core po stronie dawcy oraz atomowy import
//       baseline'u po stronie joinera (jedna transakcja SQLite + adopt epoch).
//       Transport iroh (streaming chunkow) podpinany jest w kroku 2.
// =============================================================================
//
// Niezaleznie instalowane nody maja globalne UUID (faza B). Pairing laczy dwa
// takie nody w jedna logiczna organizacje: joiner przejmuje baseline tabel
// platformowych od wybranego DAWCY i dolacza do jego org. Ten modul realizuje
// czysta (testowalna in-process) logike: kto jest dawca, jak serializowany jest
// snapshot, i jak joiner atomowo wchlania go do swojej bazy bez kolizji UNIQUE.

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use tentaflow_protocol::mesh::{BaselineAck, BaselineChunk, BaselineEpoch, BaselineHeader};
use tracing::{info, warn};

use crate::db::{self, DbPool};
use crate::sync::ledger::{LedgerResult, SyncLedgerError};

/// Klucz w `settings` trzymajacy persystowany single-flight stan adopcji.
/// Jeden wiersz na nod — adopcja jest globalna dla noda, nie per-peer, bo nod
/// moze byc w danym momencie albo dawca, albo joinerem, nigdy obojgiem.
pub const BASELINE_ADOPT_STATE_KEY: &str = "baseline_adopt_state";

/// Klucz w `settings` trzymajacy ostatni raport importu baseline'u. Zapisywany
/// po `finish_post_commit` (faza `Completed`), zeby admin mogl odpytac wynik
/// adopcji przez protokol nawet po restarcie. Czyszczony razem ze stanem adopcji.
pub const BASELINE_ADOPT_REPORT_KEY: &str = "baseline_adopt_report";

/// Maksymalny rozmiar pojedynczego chunka baseline'u (bajty surowego CBOR).
/// Dobrany pod limit ramki iroh pairingu (64 KiB) z zapasem na naglowek CBOR
/// `BaselineChunk` (seq + 32-bajtowy hash + length-prefix bytes).
pub const BASELINE_CHUNK_BYTES: usize = 48 * 1024;

/// TTL for an armed-only adopt state (phase `Elected`, no transfer ever ran).
/// An armed slot whose counterpart vanished (e.g. the joiner never pulled the
/// baseline) must not wedge the mesh: epoch-reconcile self-heal depends on this
/// slot freeing itself so later adopts toward/through this node can proceed.
const BASELINE_ADOPT_ARMED_TTL_SECS: i64 = 10 * 60;

/// TTL for any other non-terminal phase (an active transfer that died
/// mid-flight). Longer than the armed TTL because a live transfer of a large
/// snapshot may legitimately take a while between phase persists.
const BASELINE_ADOPT_ACTIVE_TTL_SECS: i64 = 60 * 60;

// =============================================================================
// Single-flight stan adopcji (crash-recovery)
// =============================================================================

/// Rola lokalnego noda w trwajacej adopcji. Nod w roli `Donor` odmawia startu
/// jako `Joiner` i odwrotnie, dopoki stan nie zostanie wyczyszczony — to chroni
/// przed split-brain (A-joins-B && B-joins-A jednoczesnie).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BaselineRole {
    Donor,
    Joiner,
}

/// Faza maszyny stanow adopcji. Persystowana po kazdym przejsciu, dzieki czemu
/// restart w trakcie transferu wie, gdzie wznowic, a powtorny import (re-pair)
/// jest idempotentny — `Completed` blokuje drugi destrukcyjny przebieg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BaselinePhase {
    /// Role uzgodnione, transfer jeszcze nie ruszyl.
    Elected,
    /// Joiner odbiera/sklada chunki snapshotu.
    Receiving,
    /// Snapshot kompletny, import do SQLite trwa albo zaraz ruszy.
    Importing,
    /// Import (merge tabel) zacommitowany TRWALE, ale post-commit adopcja epocha
    /// + reseed jeszcze nie zakonczona. Krytyczne dla idempotencji: po awarii na
    /// kroku epoch-adopt baza jest juz scalona, wiec re-pair NIE moze importowac
    /// drugi raz — musi tylko wznowic epoch-adopt+reseed do `Completed`.
    Imported,
    /// Import zacommitowany, epoch zaadoptowany — adopcja zakonczona.
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineAdoptState {
    pub role: BaselineRole,
    /// node_id drugiej strony (peer) tej adopcji.
    pub peer: String,
    /// Epoch dawcy, ktory joiner adoptuje (albo ktory dawca eksportuje).
    pub epoch: BaselineEpoch,
    pub phase: BaselinePhase,
}

/// Zapisuje stan adopcji single-flight. Jeden wiersz w `settings` per nod.
pub fn store_adopt_state(db: &DbPool, state: &BaselineAdoptState) -> LedgerResult<()> {
    let json = serde_json::to_string(state)
        .map_err(|e| SyncLedgerError::Codec(format!("baseline adopt state encode: {e}")))?;
    db::repository::set_setting(db, BASELINE_ADOPT_STATE_KEY, &json)
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))
}

/// Zapisuje stan adopcji single-flight W OBREBIE przekazanej transakcji. Uzywane
/// przez import baseline'u, by faza `Imported` byla utrwalona ATOMOWO z merge'em
/// tabel (ten sam `tx.commit()`). Bez tego istnialo okno: commit merge'a, potem
/// osobny zapis fazy — crash pomiedzy zostawial DB scalony z faza `Importing`,
/// wiec re-pair importowal drugi raz.
fn store_adopt_state_tx(tx: &Transaction<'_>, state: &BaselineAdoptState) -> LedgerResult<()> {
    let json = serde_json::to_string(state)
        .map_err(|e| SyncLedgerError::Codec(format!("baseline adopt state encode: {e}")))?;
    tx.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = datetime('now')",
        params![BASELINE_ADOPT_STATE_KEY, json],
    )
    .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    Ok(())
}

pub fn load_adopt_state(db: &DbPool) -> LedgerResult<Option<BaselineAdoptState>> {
    let Some(json) = db::repository::get_setting(db, BASELINE_ADOPT_STATE_KEY)
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?
    else {
        return Ok(None);
    };
    let state = serde_json::from_str(&json)
        .map_err(|e| SyncLedgerError::Decode(format!("baseline adopt state decode: {e}")))?;
    Ok(Some(state))
}

pub fn clear_adopt_state(db: &DbPool) -> LedgerResult<()> {
    db::repository::delete_setting(db, BASELINE_ADOPT_STATE_KEY)
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    db::repository::delete_setting(db, BASELINE_ADOPT_REPORT_KEY)
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))
}

/// Persystuje raport importu po zakonczonej adopcji, by admin mogl go odpytac
/// przez protokol nawet po restarcie noda.
pub fn store_adopt_report(db: &DbPool, report: &BaselineImportReport) -> LedgerResult<()> {
    let json = serde_json::to_string(report)
        .map_err(|e| SyncLedgerError::Codec(format!("baseline adopt report encode: {e}")))?;
    db::repository::set_setting(db, BASELINE_ADOPT_REPORT_KEY, &json)
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))
}

pub fn load_adopt_report(db: &DbPool) -> LedgerResult<Option<BaselineImportReport>> {
    let Some(json) = db::repository::get_setting(db, BASELINE_ADOPT_REPORT_KEY)
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?
    else {
        return Ok(None);
    };
    let report = serde_json::from_str(&json)
        .map_err(|e| SyncLedgerError::Decode(format!("baseline adopt report decode: {e}")))?;
    Ok(Some(report))
}

/// Czy istniejacy stan adopcji blokuje nowy start celujacy w
/// `(desired, peer, epoch)`. Dowolna trwajaca adopcja (faza != Completed) blokuje
/// KAZDY nowy start o innym celu — niezaleznie od roli/peera/epocha. To
/// szczelniejsza bramka single-flight niz "tylko przeciwna rola": dwa starty tej
/// samej roli z roznymi peerami (albo ten sam peer/inny epoch) nie moga juz oba
/// wygrac, bo pierwszy zajmuje slot, a drugi widzi konflikt. Identyczny cel
/// (`role+peer+epoch`) NIE jest konfliktem — to wznowienie. `Completed` zwalnia
/// slot (poprzednia adopcja skonczona). Czysta funkcja decyzyjna — bez I/O.
fn conflicts_with(
    existing: &BaselineAdoptState,
    desired: BaselineRole,
    peer: &str,
    epoch: &BaselineEpoch,
) -> bool {
    if existing.phase == BaselinePhase::Completed {
        return false;
    }
    let same_target = existing.role == desired && existing.peer == peer && &existing.epoch == epoch;
    if same_target {
        return false;
    }
    // An ARMED slot (`Elected`) for the SAME peer+epoch may flip role. The auto-
    // pairing path arms BOTH sides toward each other and then both dial; the content
    // election in the transport settles who actually donates. So a node armed as
    // JOINER toward a peer may still need to act as DONOR when that peer dials it
    // (the peer turned out to be the empty one), and a node armed as DONOR may need
    // to become JOINER when it decides to pull. `Elected` means "armed, no transfer
    // started", so the flip is safe. This is narrow: only the `Elected` phase, only
    // the same peer+epoch — an active transfer (Receiving/Importing/Imported) or a
    // different peer/epoch still conflicts, so split-brain protection is intact.
    if existing.peer == peer && &existing.epoch == epoch && existing.phase == BaselinePhase::Elected
    {
        return false;
    }
    true
}

/// Age of the persisted adopt state in seconds, derived from
/// `settings.updated_at` (SQLite `datetime('now')` format, UTC). `None` when the
/// timestamp is missing or unparseable — callers treat that as stale.
fn adopt_state_age_secs(updated_at: Option<&str>) -> Option<i64> {
    let parsed = chrono::NaiveDateTime::parse_from_str(updated_at?, "%Y-%m-%d %H:%M:%S").ok()?;
    Some((chrono::Utc::now().naive_utc() - parsed).num_seconds())
}

/// Whether a CONFLICTING adopt state is stale and may be evicted. Pure decision
/// function — the age is computed by the caller. An armed adopt whose
/// counterpart vanished (e.g. the joiner never pulled the baseline) must not
/// wedge the single-flight slot forever: mesh self-heal via epoch-reconcile
/// depends on this slot freeing itself. A missing/unparseable timestamp
/// (`None`) counts as stale — a corrupt timestamp must not wedge the slot.
fn is_stale_adopt_state(phase: BaselinePhase, age_secs: Option<i64>) -> bool {
    let Some(age) = age_secs else {
        return true;
    };
    let ttl = match phase {
        BaselinePhase::Elected => BASELINE_ADOPT_ARMED_TTL_SECS,
        _ => BASELINE_ADOPT_ACTIVE_TTL_SECS,
    };
    age > ttl
}

/// Atomowy start adopcji single-flight. Sprawdzenie istniejacego stanu I zapis
/// nowego stanu dziela jedna transakcje SQLite na wspoldzielonym (Mutex)
/// polaczeniu, wiec dwa rownolegle starty nie moga oba przejsc bramki: pierwszy
/// commituje stan, drugi widzi go w tej samej serializowanej transakcji i
/// dostaje twardy blad. To zamyka okno TOCTOU miedzy "czytaj rola" a "zapisz
/// rola", ktore w rozdzielonym guard+store pozwalalo na split-brain
/// (A-joins-B && B-joins-A jednoczesnie).
///
/// Idempotencja re-pair: gdy istniejacy stan dotyczy TEGO SAMEGO peera+epocha i
/// jest juz w fazie `Imported`/`Completed`, NIE jest to konflikt — zwracamy
/// `BeginOutcome::Resume(existing)`, by wywolujacy wznowil (nie restartowal)
/// adopcje. Swiezy start zwraca `BeginOutcome::Started`.
pub enum BeginOutcome {
    Started,
    Resume(BaselineAdoptState),
}

pub fn begin_adopt_atomic(
    db: &DbPool,
    desired: BaselineRole,
    peer: &str,
    epoch: &BaselineEpoch,
    phase: BaselinePhase,
) -> LedgerResult<BeginOutcome> {
    let mut conn = db::repository::acquire_for_baseline(db)
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    let tx = conn
        .transaction()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;

    let existing: Option<(BaselineAdoptState, Option<String>)> = tx
        .query_row(
            "SELECT value, updated_at FROM settings WHERE key = ?1",
            params![BASELINE_ADOPT_STATE_KEY],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?
        .map(|(json, updated_at)| {
            serde_json::from_str(&json).map(|state: BaselineAdoptState| (state, updated_at))
        })
        .transpose()
        .map_err(|e| SyncLedgerError::Decode(format!("baseline adopt state decode: {e}")))?;

    if let Some((existing, updated_at)) = existing {
        // Wznowienie tej samej adopcji (same peer+epoch+rola) w fazie
        // przetrwalej awarie post-commit lub juz zakonczonej: DB jest scalony,
        // wiec wywolujacy ma tylko dokonczyc post-commit, nie importowac od nowa.
        let same_target =
            existing.role == desired && existing.peer == peer && &existing.epoch == epoch;
        if same_target
            && matches!(
                existing.phase,
                BaselinePhase::Imported | BaselinePhase::Completed
            )
        {
            return Ok(BeginOutcome::Resume(existing));
        }
        // Single-flight: KAZDA trwajaca adopcja o innym celu (inna rola/peer/epoch,
        // faza != Completed) blokuje nowy start. Tylko identyczny cel (wznowienie
        // wczesnej fazy) lub poprzedni `Completed` przepuszczaja dalej. A stale
        // conflicting state (counterpart vanished, TTL exceeded) is EVICTED: it is
        // simply overwritten by the new state within this same transaction.
        if conflicts_with(&existing, desired, peer, epoch) {
            let age_secs = adopt_state_age_secs(updated_at.as_deref());
            if is_stale_adopt_state(existing.phase, age_secs) {
                warn!(
                    role = ?existing.role,
                    peer = %existing.peer,
                    epoch = existing.epoch.counter,
                    phase = ?existing.phase,
                    age_secs = ?age_secs,
                    "baseline adopt: evicting stale single-flight state \
                     (counterpart never completed); slot taken over by new adopt"
                );
            } else {
                return Err(SyncLedgerError::Runtime(format!(
                    "baseline adopt already in progress as {:?} with peer {} epoch {} (phase {:?}); \
                     refusing to start as {:?} with peer {} epoch {}",
                    existing.role,
                    existing.peer,
                    existing.epoch.counter,
                    existing.phase,
                    desired,
                    peer,
                    epoch.counter
                )));
            }
        }
    }

    let state = BaselineAdoptState {
        role: desired,
        peer: peer.to_string(),
        epoch: epoch.clone(),
        phase,
    };
    let json = serde_json::to_string(&state)
        .map_err(|e| SyncLedgerError::Codec(format!("baseline adopt state encode: {e}")))?;
    tx.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = datetime('now')",
        params![BASELINE_ADOPT_STATE_KEY, json],
    )
    .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    tx.commit()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    Ok(BeginOutcome::Started)
}

// =============================================================================
// Elekcja dawcy (deterministyczna, anty-split-brain)
// =============================================================================

/// Deterministyczny wybor rol. Niezaleznie od tego, ktora strona zainicjowala,
/// obie strony policza ten sam wynik z tych samych wejsc:
///   - gdy `proposed_donor` jest jednym z node_id (admin wskazal dawce) — wygrywa
///     wskazany;
///   - w przeciwnym razie LEKSYKOGRAFICZNIE NIZSZY node_id zostaje dawca.
/// Druga strona MUSI byc joinerem. Funkcja jest czysta — wlasnie ta wlasnosc
/// daje zbieznosc dual-initiate do jednego dawcy (brak A-joins-B && B-joins-A).
pub fn decide_roles(
    local_node_id: &str,
    remote_node_id: &str,
    proposed_donor: Option<&str>,
) -> (String, String) {
    let donor = match proposed_donor {
        Some(d) if d == local_node_id || d == remote_node_id => d.to_string(),
        // Brak (lub niepoprawna) jawna propozycja: nizszy node_id jest dawca.
        _ => {
            if local_node_id <= remote_node_id {
                local_node_id.to_string()
            } else {
                remote_node_id.to_string()
            }
        }
    };
    let joiner = if donor == local_node_id {
        remote_node_id.to_string()
    } else {
        local_node_id.to_string()
    };
    (donor, joiner)
}

/// Data-aware donor election. The donor is the node that HOLDS MORE content
/// (ledger operation count); ties fall back to the lexicographically lower
/// node_id, so the result is deterministic and identical on both sides given the
/// same two `(node_id, op_count)` pairs. This is the rule the auto-pairing path
/// needs: a freshly installed (near-empty) node must adopt FROM the established
/// data-holder, never the reverse — donating an empty baseline over a populated
/// peer would wipe that peer's content. Returns `(donor, joiner)`.
pub fn decide_roles_by_content(
    local_node_id: &str,
    local_op_count: u64,
    remote_node_id: &str,
    remote_op_count: u64,
) -> (String, String) {
    let local_is_donor = match local_op_count.cmp(&remote_op_count) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        // Equal content: deterministic node_id tie-break, same on both sides.
        std::cmp::Ordering::Equal => local_node_id <= remote_node_id,
    };
    if local_is_donor {
        (local_node_id.to_string(), remote_node_id.to_string())
    } else {
        (remote_node_id.to_string(), local_node_id.to_string())
    }
}

/// Lokalna rola wynikajaca z elekcji.
pub fn local_role(local_node_id: &str, donor: &str) -> BaselineRole {
    if local_node_id == donor {
        BaselineRole::Donor
    } else {
        BaselineRole::Joiner
    }
}

/// Sprawdza, ze ACK dawcy zgadza sie z tym, co joiner wynegocjowal lokalnie.
/// Transfer wolno zaczac dopiero, gdy obie strony ACK ten sam `(donor, joiner,
/// epoch)` — inaczej rolnik mialby niespojny obraz i mozliwy byłby split-brain.
pub fn validate_ack_agreement(
    ack: &BaselineAck,
    expected_donor: &str,
    expected_joiner: &str,
    expected_epoch_counter: u64,
) -> LedgerResult<()> {
    if !ack.accepted {
        return Err(SyncLedgerError::Runtime(format!(
            "donor {} rejected baseline adopt for joiner {}",
            expected_donor, expected_joiner
        )));
    }
    if ack.donor != expected_donor || ack.joiner != expected_joiner {
        return Err(SyncLedgerError::Runtime(format!(
            "baseline ack role mismatch: got donor={} joiner={}, expected donor={} joiner={}",
            ack.donor, ack.joiner, expected_donor, expected_joiner
        )));
    }
    if ack.epoch != expected_epoch_counter {
        return Err(SyncLedgerError::Runtime(format!(
            "baseline ack epoch mismatch: got {}, expected {}",
            ack.epoch, expected_epoch_counter
        )));
    }
    Ok(())
}

// =============================================================================
// Snapshot platformowych tabel core (strona dawcy)
// =============================================================================

/// Pelny baseline platformowych tabel core, branych przy spojnym stanie (jedna
/// transakcja read na bazie dawcy). Kolejnosc tabel respektuje zaleznosci FK,
/// dzieki czemu import po stronie joinera moze wstawiac je sekwencyjnie.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineSnapshot {
    pub epoch: BaselineEpoch,
    pub organizations: Vec<OrganizationRow>,
    pub roles: Vec<RoleRow>,
    pub user_accounts: Vec<UserAccountRow>,
    pub user_groups: Vec<UserGroupRow>,
    pub group_members: Vec<GroupMemberRow>,
    pub flows: Vec<FlowRow>,
    pub flow_model_bindings: Vec<FlowModelBindingRow>,
    pub sync_policies: Vec<SyncPolicyRow>,
    pub sync_resource_acl: Vec<SyncResourceAclRow>,
    pub org_memberships: Vec<OrgMembershipRow>,
    pub sync_nodes: Vec<SyncNodeRow>,
    pub user_identity_keys: Vec<UserIdentityKeyRow>,
    pub node_user_assignments: Vec<NodeUserAssignmentRow>,
    pub sync_explicit_shares: Vec<SyncExplicitShareRow>,
    /// Allowlistowane sekrety zewnetrzne (`settings` is_secret) wyslane jako
    /// ODSZYFROWANY plaintext po juz-zaufanym kanale pairingu (donor wysyla po
    /// uzgodnieniu rol); joiner re-encryptuje wlasnym `SettingsCipher` przy
    /// imporcie. Donor-wins: wartosc dawcy nadpisuje lokalna.
    pub shared_secrets: Vec<SharedSecretRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizationRow {
    pub org_id: String,
    pub name: String,
    pub slug: String,
    pub contact_email: Option<String>,
    pub dpo_contact: Option<String>,
    pub retention_policy_json: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleRow {
    pub role_id: String,
    pub name: String,
    pub permissions_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserAccountRow {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub display_name: String,
    pub email: Option<String>,
    pub is_active: bool,
    pub is_admin: bool,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserGroupRow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMemberRow {
    pub group_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowRow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub is_default: bool,
    pub service_type: Option<String>,
    pub flow_json: String,
    pub status: String,
    pub published_model_name: Option<String>,
    /// `#[serde(default)]`: snapshots from a pre-is_system donor decode as
    /// non-system rows instead of failing the whole baseline import.
    #[serde(default)]
    pub is_system: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowModelBindingRow {
    pub id: String,
    pub flow_id: String,
    pub model_pattern: String,
    pub priority: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPolicyRow {
    pub policy_id: String,
    pub org_id: String,
    pub addon_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub mode: String,
    pub authority_node_id: Option<String>,
    pub retention_days: Option<i64>,
    pub is_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncResourceAclRow {
    pub org_id: String,
    pub addon_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub owner_user_id: Option<String>,
    pub assigned_user_id: Option<String>,
    pub department_id: Option<String>,
    pub manager_user_id: Option<String>,
    pub visibility_scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgMembershipRow {
    pub org_id: String,
    pub user_id: String,
    pub role_id: String,
    pub granted_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncNodeRow {
    pub node_id: String,
    pub public_key: String,
    pub public_key_type: String,
    pub display_name: String,
    pub node_kind: String,
    pub trust_status: String,
    pub owner_user_id: Option<String>,
    pub sync_profile: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserIdentityKeyRow {
    pub key_id: String,
    pub user_id: String,
    pub key_type: String,
    pub public_key: String,
    pub purpose: String,
    pub status: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeUserAssignmentRow {
    pub node_id: String,
    pub user_id: String,
    pub assignment_mode: String,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncExplicitShareRow {
    pub org_id: String,
    pub addon_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub action: String,
    pub granted_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedSecretRow {
    pub key: String,
    /// Plaintext sekretu (donor odszyfrowal swoim cipher przed wyslaniem;
    /// joiner re-encryptuje przy imporcie). Nigdy nie persystowany w tej formie.
    pub value: String,
}

/// Buduje snapshot baseline'u z bazy dawcy w JEDNEJ transakcji read, dzieki
/// czemu wszystkie tabele widza spojny migawkowy stan (deferred-read snapshot
/// izolacji SQLite).
pub fn capture_baseline_snapshot(
    db: &DbPool,
    epoch: BaselineEpoch,
    cipher: &crate::crypto::SettingsCipher,
) -> LedgerResult<BaselineSnapshot> {
    let mut conn = db::repository::acquire_for_baseline(db)
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    let tx = conn
        .transaction()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;

    let snapshot = capture_baseline_snapshot_tx(&tx, epoch, cipher)?;
    // Read-only transakcja — commit zwalnia migawke bez zmian.
    tx.commit()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    Ok(snapshot)
}

fn capture_baseline_snapshot_tx(
    tx: &Transaction<'_>,
    epoch: BaselineEpoch,
    cipher: &crate::crypto::SettingsCipher,
) -> LedgerResult<BaselineSnapshot> {
    let map_err = |e: rusqlite::Error| SyncLedgerError::Runtime(e.to_string());

    let mut stmt = tx
        .prepare(
            "SELECT org_id, name, slug, contact_email, dpo_contact, \
                    retention_policy_json, status FROM organizations",
        )
        .map_err(map_err)?;
    let organizations = stmt
        .query_map([], |r| {
            Ok(OrganizationRow {
                org_id: r.get(0)?,
                name: r.get(1)?,
                slug: r.get(2)?,
                contact_email: r.get(3)?,
                dpo_contact: r.get(4)?,
                retention_policy_json: r.get(5)?,
                status: r.get(6)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare("SELECT role_id, name, permissions_json FROM roles")
        .map_err(map_err)?;
    let roles = stmt
        .query_map([], |r| {
            Ok(RoleRow {
                role_id: r.get(0)?,
                name: r.get(1)?,
                permissions_json: r.get(2)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare(
            "SELECT id, username, password_hash, display_name, email, is_active, is_admin, \
                    role FROM user_accounts",
        )
        .map_err(map_err)?;
    let user_accounts = stmt
        .query_map([], |r| {
            Ok(UserAccountRow {
                id: r.get(0)?,
                username: r.get(1)?,
                password_hash: r.get(2)?,
                display_name: r.get(3)?,
                email: r.get(4)?,
                is_active: r.get(5)?,
                is_admin: r.get(6)?,
                role: r.get(7)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare("SELECT id, name, description FROM user_groups")
        .map_err(map_err)?;
    let user_groups = stmt
        .query_map([], |r| {
            Ok(UserGroupRow {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare("SELECT group_id, user_id FROM group_members")
        .map_err(map_err)?;
    let group_members = stmt
        .query_map([], |r| {
            Ok(GroupMemberRow {
                group_id: r.get(0)?,
                user_id: r.get(1)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare(
            "SELECT id, name, description, is_default, service_type, flow_json, status, \
                    published_model_name, is_system FROM flows",
        )
        .map_err(map_err)?;
    let flows = stmt
        .query_map([], |r| {
            Ok(FlowRow {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                is_default: r.get(3)?,
                service_type: r.get(4)?,
                flow_json: r.get(5)?,
                status: r.get(6)?,
                published_model_name: r.get(7)?,
                is_system: r.get(8)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare("SELECT id, flow_id, model_pattern, priority FROM flow_model_bindings")
        .map_err(map_err)?;
    let flow_model_bindings = stmt
        .query_map([], |r| {
            Ok(FlowModelBindingRow {
                id: r.get(0)?,
                flow_id: r.get(1)?,
                model_pattern: r.get(2)?,
                priority: r.get(3)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare(
            "SELECT policy_id, org_id, addon_id, resource_type, resource_id, mode, \
                    authority_node_id, retention_days, is_enabled FROM sync_policies",
        )
        .map_err(map_err)?;
    let sync_policies = stmt
        .query_map([], |r| {
            Ok(SyncPolicyRow {
                policy_id: r.get(0)?,
                org_id: r.get(1)?,
                addon_id: r.get(2)?,
                resource_type: r.get(3)?,
                resource_id: r.get(4)?,
                mode: r.get(5)?,
                authority_node_id: r.get(6)?,
                retention_days: r.get(7)?,
                is_enabled: r.get(8)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare(
            "SELECT org_id, addon_id, resource_type, resource_id, owner_user_id, \
                    assigned_user_id, department_id, manager_user_id, visibility_scope \
             FROM sync_resource_acl",
        )
        .map_err(map_err)?;
    let sync_resource_acl = stmt
        .query_map([], |r| {
            Ok(SyncResourceAclRow {
                org_id: r.get(0)?,
                addon_id: r.get(1)?,
                resource_type: r.get(2)?,
                resource_id: r.get(3)?,
                owner_user_id: r.get(4)?,
                assigned_user_id: r.get(5)?,
                department_id: r.get(6)?,
                manager_user_id: r.get(7)?,
                visibility_scope: r.get(8)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare("SELECT org_id, user_id, role_id, granted_by FROM org_memberships")
        .map_err(map_err)?;
    let org_memberships = stmt
        .query_map([], |r| {
            Ok(OrgMembershipRow {
                org_id: r.get(0)?,
                user_id: r.get(1)?,
                role_id: r.get(2)?,
                granted_by: r.get(3)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare(
            "SELECT node_id, public_key, public_key_type, display_name, node_kind, \
                    trust_status, owner_user_id, sync_profile FROM sync_nodes",
        )
        .map_err(map_err)?;
    let sync_nodes = stmt
        .query_map([], |r| {
            Ok(SyncNodeRow {
                node_id: r.get(0)?,
                public_key: r.get(1)?,
                public_key_type: r.get(2)?,
                display_name: r.get(3)?,
                node_kind: r.get(4)?,
                trust_status: r.get(5)?,
                owner_user_id: r.get(6)?,
                sync_profile: r.get(7)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare(
            "SELECT key_id, user_id, key_type, public_key, purpose, status, revoked_at \
             FROM user_identity_keys",
        )
        .map_err(map_err)?;
    let user_identity_keys = stmt
        .query_map([], |r| {
            Ok(UserIdentityKeyRow {
                key_id: r.get(0)?,
                user_id: r.get(1)?,
                key_type: r.get(2)?,
                public_key: r.get(3)?,
                purpose: r.get(4)?,
                status: r.get(5)?,
                revoked_at: r.get(6)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare("SELECT node_id, user_id, assignment_mode, created_by FROM node_user_assignments")
        .map_err(map_err)?;
    let node_user_assignments = stmt
        .query_map([], |r| {
            Ok(NodeUserAssignmentRow {
                node_id: r.get(0)?,
                user_id: r.get(1)?,
                assignment_mode: r.get(2)?,
                created_by: r.get(3)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    let mut stmt = tx
        .prepare(
            "SELECT org_id, addon_id, resource_type, resource_id, subject_type, subject_id, \
                    action, granted_by FROM sync_explicit_shares WHERE revoked_at IS NULL",
        )
        .map_err(map_err)?;
    let sync_explicit_shares = stmt
        .query_map([], |r| {
            Ok(SyncExplicitShareRow {
                org_id: r.get(0)?,
                addon_id: r.get(1)?,
                resource_type: r.get(2)?,
                resource_id: r.get(3)?,
                subject_type: r.get(4)?,
                subject_id: r.get(5)?,
                action: r.get(6)?,
                granted_by: r.get(7)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    drop(stmt);

    // Sekrety: odszyfrowane plaintext do wyslania po juz-zaufanym kanale.
    let mut shared_secrets = Vec::new();
    for &key in db::repository::SHARED_SECRET_SETTING_KEYS {
        let raw: Option<String> = tx
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?;
        let Some(raw_value) = raw else { continue };
        if raw_value.is_empty() {
            continue;
        }
        let value = cipher
            .decrypt(&raw_value)
            .map_err(|e| SyncLedgerError::Runtime(format!("decrypt shared secret {key}: {e}")))?;
        shared_secrets.push(SharedSecretRow {
            key: key.to_string(),
            value,
        });
    }

    Ok(BaselineSnapshot {
        epoch,
        organizations,
        roles,
        user_accounts,
        user_groups,
        group_members,
        flows,
        flow_model_bindings,
        sync_policies,
        sync_resource_acl,
        org_memberships,
        sync_nodes,
        user_identity_keys,
        node_user_assignments,
        sync_explicit_shares,
        shared_secrets,
    })
}

/// Liczba importowanych tabel — uzywane przez naglowek transferu (`tables`).
pub const BASELINE_TABLE_NAMES: &[&str] = &[
    "organizations",
    "roles",
    "user_accounts",
    "user_groups",
    "group_members",
    "flows",
    "flow_model_bindings",
    "sync_policies",
    "sync_resource_acl",
    "org_memberships",
    "sync_nodes",
    "user_identity_keys",
    "node_user_assignments",
    "sync_explicit_shares",
    "settings",
];

// =============================================================================
// Serializacja + chunkowanie (transfer-agnostyczne, krok 2 doklada iroh)
// =============================================================================

pub fn serialize_snapshot(snapshot: &BaselineSnapshot) -> LedgerResult<Vec<u8>> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(snapshot, &mut bytes)
        .map_err(|e| SyncLedgerError::Codec(format!("baseline snapshot encode: {e}")))?;
    Ok(bytes)
}

pub fn deserialize_snapshot(bytes: &[u8]) -> LedgerResult<BaselineSnapshot> {
    ciborium::de::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| SyncLedgerError::Decode(format!("baseline snapshot decode: {e}")))
}

/// Dzieli surowy snapshot na chunki o staloj wielkosci, kazdy z hashem swojej
/// zawartosci. Hash jest weryfikowany przy skladaniu — uszkodzony chunk jest
/// wykrywany zanim snapshot trafi do importu.
pub fn chunk_snapshot(bytes: &[u8]) -> Vec<BaselineChunk> {
    bytes
        .chunks(BASELINE_CHUNK_BYTES)
        .enumerate()
        .map(|(i, slice)| BaselineChunk {
            seq: i as u64,
            content_hash: *blake3::hash(slice).as_bytes(),
            bytes: slice.to_vec(),
        })
        .collect()
}

/// Gorny limit calego snapshotu (bajty surowego CBOR). Joiner odrzuca transfer,
/// ktorego zsumowane chunki przekraczaja ten limit — chroni przed snapshotem
/// rozdmuchanym przez zlosliwego/uszkodzonego dawce do OOM. 256 MiB to zapas
/// rzedow wielkosci ponad realny baseline platformowy single-org noda.
pub const BASELINE_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Buduje naglowek transferu dla danego snapshotu: limit, rzeczywisty rozmiar i
/// hash CALOSCI. Joiner weryfikuje wszystkie trzy przy skladaniu (`reassemble_chunks`).
pub fn build_baseline_header(snapshot: &BaselineSnapshot, raw: &[u8]) -> BaselineHeader {
    let row_counts = vec![
        snapshot.organizations.len() as u64,
        snapshot.roles.len() as u64,
        snapshot.user_accounts.len() as u64,
        snapshot.user_groups.len() as u64,
        snapshot.group_members.len() as u64,
        snapshot.flows.len() as u64,
        snapshot.flow_model_bindings.len() as u64,
        snapshot.sync_policies.len() as u64,
        snapshot.sync_resource_acl.len() as u64,
        snapshot.org_memberships.len() as u64,
        snapshot.sync_nodes.len() as u64,
        snapshot.user_identity_keys.len() as u64,
        snapshot.node_user_assignments.len() as u64,
        snapshot.sync_explicit_shares.len() as u64,
        snapshot.shared_secrets.len() as u64,
    ];
    BaselineHeader {
        schema_version: 1,
        epoch: snapshot.epoch.counter,
        tables: BASELINE_TABLE_NAMES.iter().map(|s| s.to_string()).collect(),
        row_counts,
        total_bytes: raw.len() as u64,
        max_bytes: BASELINE_MAX_TOTAL_BYTES,
        content_hash: *blake3::hash(raw).as_bytes(),
    }
}

/// Sklada chunki z powrotem w surowy snapshot, egzekwujac naglowek transferu.
/// Weryfikuje: (1) ciaglosc `seq` 0..n bez luk i duplikatow, (2) `content_hash`
/// kazdego chunka (uszkodzenie w miejscu), (3) `header.max_bytes` (suma nie moze
/// przekroczyc limitu — odrzuca OOM-bomb), (4) `header.total_bytes` (skladniki
/// musza dokladnie odtworzyc deklarowany rozmiar — wykrywa ucinanie/dolepianie),
/// (5) hash CALOSCI z naglowka (wykrywa chunk z przepisanym `seq` lub
/// przestawiony, ktorego per-chunk hash sam by nie zlapal). Joiner NIGDY nie
/// importuje czesciowego/uszkodzonego/zmanipulowanego baseline'u.
pub fn reassemble_chunks(
    chunks: &[BaselineChunk],
    header: &BaselineHeader,
) -> LedgerResult<Vec<u8>> {
    if header.total_bytes > header.max_bytes || header.total_bytes > BASELINE_MAX_TOTAL_BYTES {
        return Err(SyncLedgerError::Runtime(format!(
            "baseline snapshot too large: declared {} bytes exceeds limit {} (hard cap {})",
            header.total_bytes, header.max_bytes, BASELINE_MAX_TOTAL_BYTES
        )));
    }

    let mut ordered: Vec<&BaselineChunk> = chunks.iter().collect();
    ordered.sort_by_key(|c| c.seq);

    let mut out: Vec<u8> = Vec::with_capacity(header.total_bytes as usize);
    for (expected_seq, chunk) in ordered.iter().enumerate() {
        if chunk.seq != expected_seq as u64 {
            return Err(SyncLedgerError::Runtime(format!(
                "baseline chunk sequence gap/duplicate: expected seq {expected_seq}, got {}",
                chunk.seq
            )));
        }
        let actual = *blake3::hash(&chunk.bytes).as_bytes();
        if actual != chunk.content_hash {
            return Err(SyncLedgerError::Runtime(format!(
                "baseline chunk {} content hash mismatch (corrupted transfer)",
                chunk.seq
            )));
        }
        // Egzekwuj limit calosci w trakcie skladania, by uszkodzony total_bytes
        // nie pozwolil alokowac nieograniczonej pamieci.
        if out.len() as u64 + chunk.bytes.len() as u64 > header.max_bytes {
            return Err(SyncLedgerError::Runtime(
                "baseline reassembly exceeds header max_bytes".into(),
            ));
        }
        out.extend_from_slice(&chunk.bytes);
    }

    if out.len() as u64 != header.total_bytes {
        return Err(SyncLedgerError::Runtime(format!(
            "baseline reassembly size mismatch: got {} bytes, header declared {}",
            out.len(),
            header.total_bytes
        )));
    }
    let full = *blake3::hash(&out).as_bytes();
    if full != header.content_hash {
        return Err(SyncLedgerError::Runtime(
            "baseline whole-snapshot hash mismatch (reordered/rewritten transfer)".into(),
        ));
    }
    Ok(out)
}

// =============================================================================
// Atomowy import (strona joinera)
// =============================================================================

/// Wynik importu — co dokladnie zostalo zmapowane/scalone. Uzywane przez UX
/// kroku 3 do pokazania podsumowania adopcji.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineImportReport {
    pub donor_org_id: String,
    /// Lokalni userzy joinera zmapowani na usera dawcy (match po dokladnym emailu).
    pub users_merged_by_email: usize,
    /// Lokalni userzy joinera dolaczeni jako nowi czlonkowie org dawcy.
    pub users_joined_donor_org: usize,
    /// Kolizje UNIQUE rozwiazane na korzysc dawcy (rekord joinera dostal suffix).
    pub collisions_suffixed: usize,
}

/// Atomowy import baseline'u dawcy do bazy joinera. CALOSC w jednej transakcji
/// SQLite: bledny krok cofa transakcje i zostawia joinera nietknietego.
///
/// Kolejnosc operacji:
///   (a) upsert wierszy dawcy po UUID PK — deterministyczne seedy (role,
///       org-default) zlewaja sie po tym samym id; user-created dawcy wstawiane;
///   (b) polityka kolizji UNIQUE: DAWCA wygrywa, kolidujacy rekord JOINERA
///       dostaje deterministyczny suffix (albo unpublish dla model name);
///   (c) remap lokalnych danych joinera do org dawcy: org_memberships,
///       sync_user_org_profiles, sync_resource_acl, node_user_assignments —
///       match po dokladnym emailu mapuje na usera dawcy, inaczej user joinera
///       zostaje nowym czlonkiem org dawcy;
///   (d) po COMMIT (poza transakcja): reset partycji core + adopt epoch dawcy +
///       reseed ze stanu biezacego (tylko gdy runtime sync jest aktywny).
///
/// Idempotencja: faza `Completed` w `baseline_adopt_state` blokuje powtorny
/// destrukcyjny przebieg przy re-pair.
pub fn import_baseline(
    db: &DbPool,
    snapshot: &BaselineSnapshot,
    donor_node_id: &str,
    local_node_id: &str,
    cipher: &crate::crypto::SettingsCipher,
) -> LedgerResult<BaselineImportReport> {
    let donor_org_id = primary_donor_org(snapshot)?;

    // Atomowy single-flight: sprawdzenie+zapis w jednej transakcji. `Resume`
    // oznacza, ze ta sama adopcja jest juz `Imported`/`Completed` — DB scalony,
    // wiec NIE importujemy drugi raz; wznawiamy tylko post-commit kroki.
    match begin_adopt_atomic(
        db,
        BaselineRole::Joiner,
        donor_node_id,
        &snapshot.epoch,
        BaselinePhase::Importing,
    )? {
        BeginOutcome::Started => {}
        BeginOutcome::Resume(existing) => {
            return resume_post_commit(
                db,
                &snapshot.epoch,
                donor_node_id,
                &donor_org_id,
                existing.phase,
            );
        }
    }

    // (a-c) Caly merge tabel w JEDNEJ transakcji: bledny krok cofa wszystko i
    // zostawia joinera nietknietego. Guard polaczenia scope'owany do bloku, by
    // zwolnic Mutex przed kolejnymi `acquire` (std Mutex nie jest reentrant).
    let report = {
        let mut conn = db::repository::acquire_for_baseline(db)
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        let tx = conn
            .transaction()
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;

        let report = match import_baseline_tx(&tx, snapshot, &donor_org_id, local_node_id, cipher) {
            Ok(report) => report,
            Err(e) => {
                // Rollback automatyczny przy drop(tx). Stan zostaje `Importing`
                // (faza < Imported), co znaczy "DB jeszcze NIE scalony" — re-pair
                // wystartuje pelny import od nowa. Joiner nietkniety.
                warn!(donor = %donor_node_id, "baseline import failed before commit, rolling back: {e}");
                return Err(e);
            }
        };

        // Faza `Imported` zapisana W TEJ SAMEJ TRANSAKCJI co merge tabel: commit
        // ATOMOWO scala DB i oznacza go jako zaimportowany. Brak okna miedzy
        // commitem merge'a a zapisem fazy — crash po commicie widzi `Imported`
        // (re-pair wznawia tylko post-commit), crash przed commitem cofa wszystko
        // (faza < Imported -> pelny re-import).
        store_adopt_state_tx(
            &tx,
            &BaselineAdoptState {
                role: BaselineRole::Joiner,
                peer: donor_node_id.to_string(),
                epoch: snapshot.epoch.clone(),
                phase: BaselinePhase::Imported,
            },
        )?;

        tx.commit()
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        report
    };

    finish_post_commit(db, &snapshot.epoch, donor_node_id)?;

    // Persist the completed report so admin can query the adoption outcome via
    // the binary protocol after the run (and across restarts). Failure here is
    // non-fatal: the destructive merge already committed.
    if let Err(e) = store_adopt_report(db, &report) {
        warn!("baseline import: persisting adopt report failed: {}", e);
    }

    info!(
        donor = %donor_node_id,
        donor_org = %report.donor_org_id,
        merged_by_email = report.users_merged_by_email,
        joined_donor_org = report.users_joined_donor_org,
        collisions = report.collisions_suffixed,
        "baseline import committed"
    );
    Ok(report)
}

/// Wznawia adopcje, ktorej merge tabel JUZ sie zakonczyl (faza
/// `Imported`/`Completed`). Idempotentne: dla `Completed` no-op; dla `Imported`
/// dokancza tylko post-commit epoch-adopt+reseed. NIE dotyka tabel — baza jest
/// juz scalona, ponowny import zdublowalby remapy i nadpisalby suffiksy.
fn resume_post_commit(
    db: &DbPool,
    epoch: &BaselineEpoch,
    donor_node_id: &str,
    donor_org_id: &str,
    phase: BaselinePhase,
) -> LedgerResult<BaselineImportReport> {
    let report = BaselineImportReport {
        donor_org_id: donor_org_id.to_string(),
        ..Default::default()
    };
    match phase {
        BaselinePhase::Completed => {
            info!(
                donor = %donor_node_id,
                "baseline already completed for this donor+epoch; no-op (idempotent re-pair)"
            );
            Ok(report)
        }
        BaselinePhase::Imported => {
            info!(
                donor = %donor_node_id,
                "baseline DB already merged (phase Imported); resuming epoch-adopt/reseed only"
            );
            finish_post_commit(db, epoch, donor_node_id)?;
            Ok(report)
        }
        other => Err(SyncLedgerError::Runtime(format!(
            "resume_post_commit called in unexpected phase {other:?}"
        ))),
    }
}

/// Post-commit: adoptuj epoch dawcy + reseed, potem utrwal `Completed`. Wymaga
/// aktywnego runtime sync (Fjall ledger). W testach in-process (gole DbPool bez
/// runtime) `adopt_donor_baseline_epoch` jest no-opem — transakcja SQLite jest
/// juz zatwierdzona i w pelni testowalna. Repeatable: reseed czyta scalony stan
/// SQLite, wiec ponowne wywolanie (re-pair) emituje ten sam SCALONY baseline.
fn finish_post_commit(
    db: &DbPool,
    epoch: &BaselineEpoch,
    donor_node_id: &str,
) -> LedgerResult<BaselineImportReport> {
    if let Err(e) = crate::sync::runtime::adopt_donor_baseline_epoch(epoch) {
        warn!(donor = %donor_node_id, "baseline epoch adopt/reseed failed post-commit: {e}");
        return Err(e);
    }
    store_adopt_state(
        db,
        &BaselineAdoptState {
            role: BaselineRole::Joiner,
            peer: donor_node_id.to_string(),
            epoch: epoch.clone(),
            phase: BaselinePhase::Completed,
        },
    )?;
    Ok(BaselineImportReport::default())
}

/// Org dawcy, do ktorej joiner dolacza. Snapshot MUSI miec dokladnie jedna
/// nie-`deleted` organizacje (faza C laczy DWA single-org nody). Wiecej niz jedna
/// jest twardym bledem: `drop_foreign_org_rows` kasuje wszystko spoza wybranej
/// org, wiec po cichym wyborze "najnizszego org_id" import skasowalby swiezo
/// zaimportowane wiersze pozostalych orgow dawcy. Lepiej odrzucic niz zniszczyc.
fn primary_donor_org(snapshot: &BaselineSnapshot) -> LedgerResult<String> {
    let active: Vec<&OrganizationRow> = snapshot
        .organizations
        .iter()
        .filter(|o| o.status != "deleted")
        .collect();
    match active.as_slice() {
        [only] => Ok(only.org_id.clone()),
        [] => Err(SyncLedgerError::Runtime(
            "baseline snapshot carries no active organization".into(),
        )),
        many => Err(SyncLedgerError::Runtime(format!(
            "baseline snapshot carries {} active organizations; phase C requires a single-org \
             donor — refusing import (multi-org adopt would drop the other orgs' rows)",
            many.len()
        ))),
    }
}

fn import_baseline_tx(
    tx: &Transaction<'_>,
    snapshot: &BaselineSnapshot,
    donor_org_id: &str,
    local_node_id: &str,
    cipher: &crate::crypto::SettingsCipher,
) -> LedgerResult<BaselineImportReport> {
    let map_err = |e: rusqlite::Error| SyncLedgerError::Runtime(e.to_string());
    let mut report = BaselineImportReport {
        donor_org_id: donor_org_id.to_string(),
        ..Default::default()
    };

    // Donor user_id-y, ktore sa UPRZYWILEJOWANE: `is_admin=1` albo zwiazane z
    // rola org-admina (membership w org dawcy z rola o uprawnieniu `org.admin`).
    // Email-match NIE moze scalic usera joinera w takie konto — niewerifikowany
    // email == przejecie konta admina. Tacy userzy joinera zostaja osobni.
    let privileged_donor_ids = privileged_donor_user_ids(snapshot, donor_org_id);

    // Email -> id usera dawcy do mapowania tozsamosci, Z POMINIECIEM kont
    // uprzywilejowanych (te nie sa celem merge'a).
    let donor_email_to_id: BTreeMap<String, String> = snapshot
        .user_accounts
        .iter()
        .filter(|u| !privileged_donor_ids.contains(u.id.as_str()))
        .filter_map(|u| {
            u.email
                .as_deref()
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(|e| (e.to_string(), u.id.clone()))
        })
        .collect();
    let donor_ids: std::collections::BTreeSet<&str> = snapshot
        .user_accounts
        .iter()
        .map(|u| u.id.as_str())
        .collect();

    // Lokalni (joinera) userzy PRZED importem — uzywane do remapu i kolizji.
    let local_users = read_local_users(tx)?;

    // (b) Kolizje UNIQUE: dawca wygrywa. Najpierw rozsuwamy kolidujace UNIQUE
    // wartosci po stronie joinera (zanim wstawimy wiersze dawcy), aby INSERT
    // dawcy nie wpadl na istniejacy lokalny rekord o tej samej wartosci.
    suffix_local_collisions(tx, snapshot, &mut report, &map_err)?;

    // (a) Upsert wierszy dawcy po UUID PK. Deterministyczne seedy (np.
    // role-org-admin, org-default) zlewaja sie po tym samym id; user-created
    // dawcy sa wstawiane jako nowe.
    upsert_donor_rows(tx, snapshot, donor_org_id, local_node_id, cipher, &map_err)?;

    // (c) Remap lokalnych danych joinera do org dawcy. Email-match mapuje na
    // usera dawcy; inaczej user joinera dolacza jako nowy czlonek org dawcy.
    for local in &local_users {
        // Lokalny user dawcy (np. lokalny wpis o tym samym id) nie jest remapowany.
        if donor_ids.contains(local.id.as_str()) {
            continue;
        }
        let local_email = local
            .email
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty());
        let mapped_donor_id = local_email.and_then(|e| donor_email_to_id.get(e));

        if let Some(donor_id) = mapped_donor_id {
            report.users_merged_by_email += 1;
            // User joinera jest TYM SAMYM czlowiekiem co (nie-uprzywilejowany)
            // user dawcy — przepinamy dane joinera na id dawcy, lokalny wiersz
            // joinera znika.
            remap_user_owned_rows(tx, &local.id, donor_id, donor_org_id, &map_err)?;
        } else {
            // Nowy czlowiek (rozny email, ALBO email rowny kontu uprzywilejowanemu
            // dawcy — wtedy nie merguje, zostaje osobny; loguj WARN). Zostaje
            // wlasnym userem, ale staje sie czlonkiem org dawcy z najmniej
            // uprzywilejowana rola.
            if let Some(email) = local_email {
                if snapshot.user_accounts.iter().any(|u| {
                    privileged_donor_ids.contains(u.id.as_str())
                        && u.email.as_deref().map(str::trim) == Some(email)
                }) {
                    warn!(
                        local_user = %local.id,
                        "baseline import: joiner email matches a PRIVILEGED donor account; \
                         refusing identity merge, joiner stays a separate member (admin-takeover guard)"
                    );
                }
            }
            report.users_joined_donor_org += 1;
            attach_local_user_to_donor_org(tx, &local.id, donor_org_id, snapshot, &map_err)?;
        }
    }

    // Po remapie usuwamy lokalne org_memberships/profile wskazujace na orgi inne
    // niz dawcy (joiner nie ma juz wlasnej org — wchlonal org dawcy).
    drop_foreign_org_rows(tx, donor_org_id, &map_err)?;

    Ok(report)
}

/// Zbior donor user_id-ow uznawanych za uprzywilejowane: `is_admin=1` LUB user
/// ma w org dawcy czlonkostwo z rola niosaca uprawnienie `org.admin`. Match po
/// emailu na takie konto NIE scala (chroni przed przejeciem konta admina).
fn privileged_donor_user_ids<'a>(
    snapshot: &'a BaselineSnapshot,
    donor_org_id: &str,
) -> std::collections::BTreeSet<&'a str> {
    let admin_role_ids: std::collections::BTreeSet<&str> = snapshot
        .roles
        .iter()
        .filter(|r| role_is_privileged(&r.permissions_json))
        .map(|r| r.role_id.as_str())
        .collect();

    let mut out: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for u in &snapshot.user_accounts {
        if u.is_admin {
            out.insert(u.id.as_str());
        }
    }
    for m in &snapshot.org_memberships {
        if m.org_id == donor_org_id && admin_role_ids.contains(m.role_id.as_str()) {
            // FK od memberships do user_accounts — bierzemy referencje na id usera.
            if let Some(u) = snapshot.user_accounts.iter().find(|u| u.id == m.user_id) {
                out.insert(u.id.as_str());
            }
        }
    }
    out
}

/// Czy rola jest uprzywilejowana (admin). Decyduje uprawnienie `org.admin` w
/// `permissions_json` — to klucz nadajacy pelna administracje organizacja.
fn role_is_privileged(permissions_json: &str) -> bool {
    serde_json::from_str::<Vec<String>>(permissions_json)
        .map(|perms| perms.iter().any(|p| p == "org.admin"))
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
struct LocalUser {
    id: String,
    email: Option<String>,
}

fn read_local_users(tx: &Transaction<'_>) -> LedgerResult<Vec<LocalUser>> {
    let map_err = |e: rusqlite::Error| SyncLedgerError::Runtime(e.to_string());
    let mut stmt = tx
        .prepare("SELECT id, email FROM user_accounts")
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(LocalUser {
                id: r.get(0)?,
                email: r.get(1)?,
            })
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;
    Ok(rows)
}

/// Maksymalna dlugosc wartosci tekstowej dla suffixowanych kolumn UNIQUE. Bierze
/// gorne ograniczenie z najwezszej realnej kolumny; suffix jest doklejany w
/// granicach tego limitu, by uniknac przepelnienia (SQLite nie egzekwuje
/// dlugosci, ale UI/inne warstwy zakladaja rozsadny limit).
const COLLISION_VALUE_MAX_LEN: usize = 200;

/// Gorna granica prob sondowania wolnej wartosci. Przy realnym imporcie kolizji
/// jest garstka; setny kolejny suffix oznacza patologie (np. ktos celowo zalal
/// przestrzen nazw) — wtedy lepiej zwrocic blad niz petlic.
const COLLISION_PROBE_LIMIT: u32 = 10_000;

/// Sonduje wolna wartosc dla kolumny UNIQUE: probuje `<base>-<short_id>`, a gdy
/// ta tez koliduje, dokleja licznik `<base>-<short_id>-<n>`. Pierwszy wariant
/// jest deterministyczny (stabilny per id), kolejne tylko gdy realnie wystepuja
/// dalsze kolizje. `exists` zwraca czy dana kandydat-wartosc jest juz zajeta
/// (przez kogokolwiek poza wlasnym wierszem).
///
/// KLUCZOWE: kandydat budowany jest tak, by ZAWSZE zmiescil sie w limicie
/// dlugosci ORAZ by licznik realnie zmienial wynik. Gdyby ucinac dopiero gotowy
/// `<base>-<short>-<n>` przy max-dlugiej bazie, ucinany bylby zmienny licznik —
/// wszystkie kandydatury bylyby identyczne i petla bieglaby do limitu. Dlatego
/// REZERWUJEMY miejsce na suffix: baze przycinamy do `max_len - suffix_len`,
/// a dopiero potem doklejamy `-{short}` / `-{short}-{counter}`.
fn probe_free_value(
    base: &str,
    local_id: &str,
    mut exists: impl FnMut(&str) -> LedgerResult<bool>,
) -> LedgerResult<String> {
    let short = short_id(local_id);

    let mut counter: u32 = 0;
    loop {
        let suffix = if counter == 0 {
            format!("-{short}")
        } else {
            format!("-{short}-{counter}")
        };
        // Zarezerwuj miejsce na suffix: baze tniemy do reszty limitu, by calosc
        // miescila sie w `COLLISION_VALUE_MAX_LEN` a licznik nie byl obciety.
        let reserved = COLLISION_VALUE_MAX_LEN.saturating_sub(suffix.chars().count());
        let base_part: String = base.chars().take(reserved).collect();
        let candidate = format!("{base_part}{suffix}");

        if !exists(&candidate)? {
            return Ok(candidate);
        }

        counter += 1;
        if counter >= COLLISION_PROBE_LIMIT {
            return Err(SyncLedgerError::Runtime(format!(
                "could not find a free unique value for base '{base}' after {COLLISION_PROBE_LIMIT} probes"
            )));
        }
    }
}

/// (b) Rozsuwa kolidujace UNIQUE wartosci joinera, gdy dawca niesie rekord o
/// innym UUID PK ale tej samej wartosci UNIQUE. Dawca wygrywa: lokalny rekord
/// joinera dostaje sondowany wolny suffix (`<value>-<short_local_id>[-n]`), a
/// `published_model_name` flowa joinera jest unpublishowany (NULL), bo to klucz
/// uzywany routingowo i suffix zmienilby semantyke modelu.
fn suffix_local_collisions(
    tx: &Transaction<'_>,
    snapshot: &BaselineSnapshot,
    report: &mut BaselineImportReport,
    map_err: &impl Fn(rusqlite::Error) -> SyncLedgerError,
) -> LedgerResult<()> {
    // user_accounts.username
    for donor in &snapshot.user_accounts {
        let local_id: Option<String> = tx
            .query_row(
                "SELECT id FROM user_accounts WHERE username = ?1 AND id <> ?2",
                params![donor.username, donor.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?;
        if let Some(local_id) = local_id {
            let suffixed = probe_free_value(&donor.username, &local_id, |cand| {
                value_taken(
                    tx,
                    "user_accounts",
                    "username",
                    "id",
                    cand,
                    &local_id,
                    map_err,
                )
            })?;
            tx.execute(
                "UPDATE user_accounts SET username = ?1 WHERE id = ?2",
                params![suffixed, local_id],
            )
            .map_err(map_err)?;
            report.collisions_suffixed += 1;
        }
    }

    // organizations.slug
    for donor in &snapshot.organizations {
        let local_id: Option<String> = tx
            .query_row(
                "SELECT org_id FROM organizations WHERE slug = ?1 AND org_id <> ?2",
                params![donor.slug, donor.org_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?;
        if let Some(local_id) = local_id {
            let suffixed = probe_free_value(&donor.slug, &local_id, |cand| {
                value_taken(
                    tx,
                    "organizations",
                    "slug",
                    "org_id",
                    cand,
                    &local_id,
                    map_err,
                )
            })?;
            tx.execute(
                "UPDATE organizations SET slug = ?1 WHERE org_id = ?2",
                params![suffixed, local_id],
            )
            .map_err(map_err)?;
            report.collisions_suffixed += 1;
        }
    }

    // roles.name
    for donor in &snapshot.roles {
        let local_id: Option<String> = tx
            .query_row(
                "SELECT role_id FROM roles WHERE name = ?1 AND role_id <> ?2",
                params![donor.name, donor.role_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?;
        if let Some(local_id) = local_id {
            let suffixed = probe_free_value(&donor.name, &local_id, |cand| {
                value_taken(tx, "roles", "name", "role_id", cand, &local_id, map_err)
            })?;
            tx.execute(
                "UPDATE roles SET name = ?1 WHERE role_id = ?2",
                params![suffixed, local_id],
            )
            .map_err(map_err)?;
            report.collisions_suffixed += 1;
        }
    }

    // user_groups.name
    for donor in &snapshot.user_groups {
        let local_id: Option<String> = tx
            .query_row(
                "SELECT id FROM user_groups WHERE name = ?1 AND id <> ?2",
                params![donor.name, donor.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?;
        if let Some(local_id) = local_id {
            let suffixed = probe_free_value(&donor.name, &local_id, |cand| {
                value_taken(tx, "user_groups", "name", "id", cand, &local_id, map_err)
            })?;
            tx.execute(
                "UPDATE user_groups SET name = ?1 WHERE id = ?2",
                params![suffixed, local_id],
            )
            .map_err(map_err)?;
            report.collisions_suffixed += 1;
        }
    }

    // flows.published_model_name — unpublish lokalnego flowa (NULL), bo to klucz
    // routingowy; suffix zmienilby nazwe modelu widoczna przez API.
    for donor in &snapshot.flows {
        let Some(model) = donor.published_model_name.as_deref() else {
            continue;
        };
        let local_id: Option<String> = tx
            .query_row(
                "SELECT id FROM flows WHERE published_model_name = ?1 AND id <> ?2",
                params![model, donor.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?;
        if let Some(local_id) = local_id {
            tx.execute(
                "UPDATE flows SET published_model_name = NULL WHERE id = ?1",
                params![local_id],
            )
            .map_err(map_err)?;
            report.collisions_suffixed += 1;
        }
    }

    // flow_model_bindings.model_pattern
    for donor in &snapshot.flow_model_bindings {
        let local_id: Option<String> = tx
            .query_row(
                "SELECT id FROM flow_model_bindings WHERE model_pattern = ?1 AND id <> ?2",
                params![donor.model_pattern, donor.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?;
        if let Some(local_id) = local_id {
            let suffixed = probe_free_value(&donor.model_pattern, &local_id, |cand| {
                value_taken(
                    tx,
                    "flow_model_bindings",
                    "model_pattern",
                    "id",
                    cand,
                    &local_id,
                    map_err,
                )
            })?;
            tx.execute(
                "UPDATE flow_model_bindings SET model_pattern = ?1 WHERE id = ?2",
                params![suffixed, local_id],
            )
            .map_err(map_err)?;
            report.collisions_suffixed += 1;
        }
    }

    Ok(())
}

/// Czy wartosc `value` w `table.column` jest juz zajeta przez wiersz INNY niz
/// `self_id` (kolumna PK `pk_column`). Uzywane przez `probe_free_value`, by
/// nowy suffix nie wpadl na kolejna istniejaca kolizje (np. inny wiersz dawcy
/// lub wczesniej wstawiony joiner).
fn value_taken(
    tx: &Transaction<'_>,
    table: &str,
    column: &str,
    pk_column: &str,
    value: &str,
    self_id: &str,
    map_err: &impl Fn(rusqlite::Error) -> SyncLedgerError,
) -> LedgerResult<bool> {
    let found: Option<i64> = tx
        .query_row(
            &format!("SELECT 1 FROM {table} WHERE {column} = ?1 AND {pk_column} <> ?2"),
            params![value, self_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_err)?;
    Ok(found.is_some())
}

/// (a) Wstawia/aktualizuje wiersze dawcy po UUID PK. INSERT ... ON CONFLICT(PK)
/// DO UPDATE — deterministyczne seedy (te same UUID) sa scalane, user-created
/// dawcy wstawiane. Kolejnosc respektuje FK: organizacje/role -> user_accounts
/// -> grupy -> czlonkostwa -> flows -> bindings -> sync_*.
fn upsert_donor_rows(
    tx: &Transaction<'_>,
    snapshot: &BaselineSnapshot,
    donor_org_id: &str,
    local_node_id: &str,
    cipher: &crate::crypto::SettingsCipher,
    map_err: &impl Fn(rusqlite::Error) -> SyncLedgerError,
) -> LedgerResult<()> {
    for o in &snapshot.organizations {
        tx.execute(
            "INSERT INTO organizations \
                (org_id, name, slug, contact_email, dpo_contact, retention_policy_json, status, \
                 created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, strftime('%Y-%m-%dT%H:%M:%SZ','now')) \
             ON CONFLICT(org_id) DO UPDATE SET \
                name = excluded.name, slug = excluded.slug, \
                contact_email = excluded.contact_email, dpo_contact = excluded.dpo_contact, \
                retention_policy_json = excluded.retention_policy_json, status = excluded.status",
            params![
                o.org_id,
                o.name,
                o.slug,
                o.contact_email,
                o.dpo_contact,
                o.retention_policy_json,
                o.status
            ],
        )
        .map_err(map_err)?;
    }

    for r in &snapshot.roles {
        tx.execute(
            "INSERT INTO roles (role_id, name, permissions_json, created_at) \
             VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ','now')) \
             ON CONFLICT(role_id) DO UPDATE SET \
                name = excluded.name, permissions_json = excluded.permissions_json",
            params![r.role_id, r.name, r.permissions_json],
        )
        .map_err(map_err)?;
    }

    for u in &snapshot.user_accounts {
        tx.execute(
            "INSERT INTO user_accounts \
                (id, username, password_hash, display_name, email, is_active, is_admin, role) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(id) DO UPDATE SET \
                username = excluded.username, password_hash = excluded.password_hash, \
                display_name = excluded.display_name, email = excluded.email, \
                is_active = excluded.is_active, is_admin = excluded.is_admin, role = excluded.role",
            params![
                u.id,
                u.username,
                u.password_hash,
                u.display_name,
                u.email,
                u.is_active,
                u.is_admin,
                u.role
            ],
        )
        .map_err(map_err)?;
    }

    for g in &snapshot.user_groups {
        tx.execute(
            "INSERT INTO user_groups (id, name, description, created_at) \
             VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ','now')) \
             ON CONFLICT(id) DO UPDATE SET \
                name = excluded.name, description = excluded.description",
            params![g.id, g.name, g.description],
        )
        .map_err(map_err)?;
    }

    for m in &snapshot.group_members {
        tx.execute(
            "INSERT INTO group_members (group_id, user_id) VALUES (?1, ?2) \
             ON CONFLICT(group_id, user_id) DO NOTHING",
            params![m.group_id, m.user_id],
        )
        .map_err(map_err)?;
    }

    for f in &snapshot.flows {
        tx.execute(
            "INSERT INTO flows \
                (id, name, description, is_default, service_type, flow_json, status, \
                 published_model_name, is_system) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT(id) DO UPDATE SET \
                name = excluded.name, description = excluded.description, \
                is_default = excluded.is_default, service_type = excluded.service_type, \
                flow_json = excluded.flow_json, status = excluded.status, \
                published_model_name = excluded.published_model_name, \
                is_system = excluded.is_system",
            params![
                f.id,
                f.name,
                f.description,
                f.is_default,
                f.service_type,
                f.flow_json,
                f.status,
                f.published_model_name,
                f.is_system
            ],
        )
        .map_err(map_err)?;
    }

    for b in &snapshot.flow_model_bindings {
        tx.execute(
            "INSERT INTO flow_model_bindings (id, flow_id, model_pattern, priority) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(id) DO UPDATE SET \
                flow_id = excluded.flow_id, model_pattern = excluded.model_pattern, \
                priority = excluded.priority",
            params![b.id, b.flow_id, b.model_pattern, b.priority],
        )
        .map_err(map_err)?;
    }

    for m in &snapshot.org_memberships {
        tx.execute(
            "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
             VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ','now'), ?4) \
             ON CONFLICT(org_id, user_id) DO UPDATE SET \
                role_id = excluded.role_id, granted_by = excluded.granted_by",
            params![m.org_id, m.user_id, m.role_id, m.granted_by],
        )
        .map_err(map_err)?;
    }

    for p in &snapshot.sync_policies {
        // sync_policies ma DWA ograniczenia: PK(policy_id) ORAZ
        // UNIQUE(org_id,addon_id,resource_type,resource_id). ON CONFLICT(policy_id)
        // nie lapie joinerowego wiersza o INNYM policy_id ale tym samym kluczu
        // logicznym (np. realny default-org). Donor-wins: kasujemy taki kolidujacy
        // wiersz joinera ZANIM wstawimy wiersz dawcy, by INSERT nie padl na UNIQUE.
        tx.execute(
            "DELETE FROM sync_policies \
             WHERE org_id = ?1 AND addon_id = ?2 AND resource_type = ?3 AND resource_id = ?4 \
               AND policy_id <> ?5",
            params![
                p.org_id,
                p.addon_id,
                p.resource_type,
                p.resource_id,
                p.policy_id
            ],
        )
        .map_err(map_err)?;
        tx.execute(
            "INSERT INTO sync_policies \
                (policy_id, org_id, addon_id, resource_type, resource_id, mode, \
                 authority_node_id, retention_days, is_enabled) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT(policy_id) DO UPDATE SET \
                org_id = excluded.org_id, addon_id = excluded.addon_id, \
                resource_type = excluded.resource_type, resource_id = excluded.resource_id, \
                mode = excluded.mode, authority_node_id = excluded.authority_node_id, \
                retention_days = excluded.retention_days, is_enabled = excluded.is_enabled",
            params![
                p.policy_id,
                p.org_id,
                p.addon_id,
                p.resource_type,
                p.resource_id,
                p.mode,
                p.authority_node_id,
                p.retention_days,
                p.is_enabled
            ],
        )
        .map_err(map_err)?;
    }

    // sync_resource_acl: PK to (org_id,addon_id,resource_type,resource_id), wiec
    // ON CONFLICT na PK juz realizuje donor-wins (wiersz dawcy nadpisuje joinera).
    // Wiersze ACL dawcy MOGA wskazywac authority_node_id/owner spoza zakresu, ale
    // FK na user_accounts jest spelnione bo userow dawcy juz wstawilismy wyzej.
    for a in &snapshot.sync_resource_acl {
        tx.execute(
            "INSERT INTO sync_resource_acl \
                (org_id, addon_id, resource_type, resource_id, owner_user_id, assigned_user_id, \
                 department_id, manager_user_id, visibility_scope) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT(org_id, addon_id, resource_type, resource_id) DO UPDATE SET \
                owner_user_id = excluded.owner_user_id, \
                assigned_user_id = excluded.assigned_user_id, \
                department_id = excluded.department_id, \
                manager_user_id = excluded.manager_user_id, \
                visibility_scope = excluded.visibility_scope",
            params![
                a.org_id,
                a.addon_id,
                a.resource_type,
                a.resource_id,
                a.owner_user_id,
                a.assigned_user_id,
                a.department_id,
                a.manager_user_id,
                a.visibility_scope
            ],
        )
        .map_err(map_err)?;
    }

    // sync_nodes: importujemy wezly dawcy (joiner poznaje klaster), ale ZACHOWUJEMY
    // wlasny wpis lokalnego node joinera — nie nadpisujemy go danymi dawcy i nie
    // kasujemy. Dawca nie zna lokalnego noda joinera, wiec brak go w snapshocie;
    // upsert po node_id wstawia tylko wezly dawcy.
    for n in &snapshot.sync_nodes {
        if n.node_id == local_node_id {
            // Teoretycznie dawca nie powinien znac lokalnego noda joinera; gdyby
            // jednak go niosl, NIE nadpisujemy wlasnego wpisu zaufania.
            continue;
        }
        tx.execute(
            "INSERT INTO sync_nodes \
                (node_id, public_key, public_key_type, display_name, node_kind, trust_status, \
                 owner_user_id, sync_profile, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, \
                     strftime('%Y-%m-%dT%H:%M:%SZ','now'), strftime('%Y-%m-%dT%H:%M:%SZ','now')) \
             ON CONFLICT(node_id) DO UPDATE SET \
                public_key = excluded.public_key, public_key_type = excluded.public_key_type, \
                display_name = excluded.display_name, node_kind = excluded.node_kind, \
                trust_status = excluded.trust_status, owner_user_id = excluded.owner_user_id, \
                sync_profile = excluded.sync_profile",
            params![
                n.node_id,
                n.public_key,
                n.public_key_type,
                n.display_name,
                n.node_kind,
                n.trust_status,
                n.owner_user_id,
                n.sync_profile
            ],
        )
        .map_err(map_err)?;
    }

    // user_identity_keys: zwiazane z userami dawcy (FK user_id -> user_accounts).
    // Userzy dawcy sa juz wstawieni; klucze lokalnego node joinera zostaja
    // nietkniete (nie ma ich w snapshocie dawcy). UNIQUE(user_id,key_type,
    // public_key) — donor-wins: kasujemy kolidujacy klucz joinera o innym key_id.
    for k in &snapshot.user_identity_keys {
        tx.execute(
            "DELETE FROM user_identity_keys \
             WHERE user_id = ?1 AND key_type = ?2 AND public_key = ?3 AND key_id <> ?4",
            params![k.user_id, k.key_type, k.public_key, k.key_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "INSERT INTO user_identity_keys \
                (key_id, user_id, key_type, public_key, purpose, status, created_at, revoked_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, strftime('%Y-%m-%dT%H:%M:%SZ','now'), ?7) \
             ON CONFLICT(key_id) DO UPDATE SET \
                user_id = excluded.user_id, key_type = excluded.key_type, \
                public_key = excluded.public_key, purpose = excluded.purpose, \
                status = excluded.status, revoked_at = excluded.revoked_at",
            params![
                k.key_id,
                k.user_id,
                k.key_type,
                k.public_key,
                k.purpose,
                k.status,
                k.revoked_at
            ],
        )
        .map_err(map_err)?;
    }

    // node_user_assignments: FK na sync_nodes(node_id) i user_accounts(id). Wezly i
    // userzy dawcy sa juz wstawieni. Przypisania lokalnego node joinera zostaja.
    for a in &snapshot.node_user_assignments {
        tx.execute(
            "INSERT INTO node_user_assignments \
                (node_id, user_id, assignment_mode, created_by, created_at) \
             VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%SZ','now')) \
             ON CONFLICT(node_id, user_id, assignment_mode) DO UPDATE SET \
                created_by = excluded.created_by",
            params![a.node_id, a.user_id, a.assignment_mode, a.created_by],
        )
        .map_err(map_err)?;
    }

    // sync_explicit_shares: to ACL. Importujemy share'y dawcy z remapem subject/
    // granted_by do org dawcy. Subjekt typu 'user' i granted_by sa juz userami
    // dawcy (z user_accounts). PK obejmuje (org,addon,type,id,subject_type,
    // subject_id,action) — ON CONFLICT realizuje donor-wins. org_id zawsze ==
    // donor_org_id (jedna org w snapshocie po `primary_donor_org`).
    for s in &snapshot.sync_explicit_shares {
        tx.execute(
            "INSERT INTO sync_explicit_shares \
                (org_id, addon_id, resource_type, resource_id, subject_type, subject_id, action, \
                 granted_by, granted_at, revoked_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, strftime('%Y-%m-%dT%H:%M:%SZ','now'), NULL) \
             ON CONFLICT(org_id, addon_id, resource_type, resource_id, subject_type, subject_id, action) \
             DO UPDATE SET granted_by = excluded.granted_by, revoked_at = NULL",
            params![
                donor_org_id,
                s.addon_id,
                s.resource_type,
                s.resource_id,
                s.subject_type,
                s.subject_id,
                s.action,
                s.granted_by
            ],
        )
        .map_err(map_err)?;
    }

    // shared secrets: donor-wins. Donor przyslal ODSZYFROWANY plaintext po
    // zaufanym kanale; re-encryptujemy lokalnym cipherem i nadpisujemy wartosc.
    // Reseed czyta z `settings`, wiec po tym imporcie emituje sekret DAWCY, nie
    // lokalny — kluczowe, by reseed nie cofnal donor-wins.
    for secret in &snapshot.shared_secrets {
        if !db::repository::is_shared_secret_setting_key(&secret.key) {
            continue;
        }
        let encrypted = cipher
            .encrypt(&secret.value)
            .map_err(|e| SyncLedgerError::Runtime(format!("encrypt shared secret: {e}")))?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = datetime('now')",
            params![secret.key, encrypted],
        )
        .map_err(map_err)?;
    }

    Ok(())
}

/// (c) Ten sam czlowiek (email-match): przepina dane joinera z `local_id` na
/// `donor_id` i kasuje lokalny wiersz usera joinera. Wszystkie FK wskazujace na
/// usera (grupy, ACL owner/assigned/manager, profile, node assignments, org
/// memberships) sa przekierowane na usera dawcy w org dawcy.
fn remap_user_owned_rows(
    tx: &Transaction<'_>,
    local_id: &str,
    donor_id: &str,
    donor_org_id: &str,
    map_err: &impl Fn(rusqlite::Error) -> SyncLedgerError,
) -> LedgerResult<()> {
    if local_id == donor_id {
        return Ok(());
    }

    // ACL owner/assigned/manager -> id dawcy, scope org dawcy.
    for column in ["owner_user_id", "assigned_user_id", "manager_user_id"] {
        tx.execute(
            &format!(
                "UPDATE sync_resource_acl SET {column} = ?1, org_id = ?2 \
                 WHERE {column} = ?3"
            ),
            params![donor_id, donor_org_id, local_id],
        )
        .map_err(map_err)?;
    }

    // node_user_assignments -> id dawcy.
    tx.execute(
        "UPDATE OR IGNORE node_user_assignments SET user_id = ?1 WHERE user_id = ?2",
        params![donor_id, local_id],
    )
    .map_err(map_err)?;
    tx.execute(
        "UPDATE OR IGNORE node_user_assignments SET created_by = ?1 WHERE created_by = ?2",
        params![donor_id, local_id],
    )
    .map_err(map_err)?;

    // sync_user_org_profiles -> id dawcy w org dawcy.
    tx.execute(
        "UPDATE OR IGNORE sync_user_org_profiles SET user_id = ?1, org_id = ?2 WHERE user_id = ?3",
        params![donor_id, donor_org_id, local_id],
    )
    .map_err(map_err)?;
    tx.execute(
        "UPDATE OR IGNORE sync_user_org_profiles SET manager_user_id = ?1 WHERE manager_user_id = ?2",
        params![donor_id, local_id],
    )
    .map_err(map_err)?;

    // group_members -> id dawcy.
    tx.execute(
        "UPDATE OR IGNORE group_members SET user_id = ?1 WHERE user_id = ?2",
        params![donor_id, local_id],
    )
    .map_err(map_err)?;

    // sync_nodes.owner_user_id -> id dawcy. Bez tego po usunieciu lokalnego usera
    // FK `ON DELETE SET NULL` zerowalby wlasciciela noda joinera.
    tx.execute(
        "UPDATE sync_nodes SET owner_user_id = ?1 WHERE owner_user_id = ?2",
        params![donor_id, local_id],
    )
    .map_err(map_err)?;

    // sync_explicit_shares: subject (gdy subject_type='user') oraz wystawca grantu
    // przepinane na dawce. Bez subject_id: FK `ON DELETE CASCADE` przy usunieciu
    // usera skasowalby udzialy; bez granted_by: `ON DELETE SET NULL` zgubilby
    // autora grantu. `OR IGNORE` na subject_id chroni przed kolizja PK, gdy dawca
    // ma juz identyczny grant (donor-wins, dublet znika ponizej).
    tx.execute(
        "UPDATE OR IGNORE sync_explicit_shares SET subject_id = ?1 \
         WHERE subject_type = 'user' AND subject_id = ?2",
        params![donor_id, local_id],
    )
    .map_err(map_err)?;
    tx.execute(
        "DELETE FROM sync_explicit_shares WHERE subject_type = 'user' AND subject_id = ?1",
        params![local_id],
    )
    .map_err(map_err)?;
    tx.execute(
        "UPDATE sync_explicit_shares SET granted_by = ?1 WHERE granted_by = ?2",
        params![donor_id, local_id],
    )
    .map_err(map_err)?;

    // user_identity_keys: donor-wins. Klucze tozsamosci dawcy sa juz
    // zaimportowane i autorytatywne dla scalonej tozsamosci, wiec lokalne klucze
    // joinera usuwamy (a nie remapujemy) — remap naruszylby UNIQUE(user_id,
    // key_type, public_key) gdy dawca ma juz klucz tego samego typu/wartosci, a
    // poza tym joinerowy klucz prywatny nie nalezy do tozsamosci dawcy.
    tx.execute(
        "DELETE FROM user_identity_keys WHERE user_id = ?1",
        params![local_id],
    )
    .map_err(map_err)?;

    // org_memberships joinera dla tego usera znikna z `drop_foreign_org_rows`;
    // usuwamy lokalny wiersz usera joinera (dane juz przepiete na dawce).
    tx.execute("DELETE FROM user_accounts WHERE id = ?1", params![local_id])
        .map_err(map_err)?;

    Ok(())
}

/// (c) Nowy czlowiek: zostaje wlasnym userem, ale staje sie czlonkiem org
/// dawcy. Dodaje org_membership w org dawcy z najmniej uprzywilejowana rola
/// dostepna w snapshocie (preferowana `role-user`, inaczej pierwsza rola).
fn attach_local_user_to_donor_org(
    tx: &Transaction<'_>,
    local_id: &str,
    donor_org_id: &str,
    snapshot: &BaselineSnapshot,
    map_err: &impl Fn(rusqlite::Error) -> SyncLedgerError,
) -> LedgerResult<()> {
    let role_id = pick_member_role(snapshot)?;
    tx.execute(
        "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
         VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ','now'), ?2) \
         ON CONFLICT(org_id, user_id) DO UPDATE SET role_id = excluded.role_id",
        params![donor_org_id, local_id, role_id],
    )
    .map_err(map_err)?;

    // Przepinamy ACL/profile/assignments owned przez tego usera na org dawcy
    // (user zostaje, zmienia sie tylko jego org).
    for column in ["owner_user_id", "assigned_user_id", "manager_user_id"] {
        tx.execute(
            &format!("UPDATE sync_resource_acl SET org_id = ?1 WHERE {column} = ?2"),
            params![donor_org_id, local_id],
        )
        .map_err(map_err)?;
    }
    tx.execute(
        "UPDATE OR IGNORE sync_user_org_profiles SET org_id = ?1 WHERE user_id = ?2",
        params![donor_org_id, local_id],
    )
    .map_err(map_err)?;

    Ok(())
}

/// Wybiera role dla nowego czlonka org dawcy. NIGDY nie nadaje roli
/// uprzywilejowanej (z `org.admin`) — niewerifikowany joiner nie moze wpasc na
/// konto admina przez przypadkowy fallback. Preferencja:
///   1. `role-user`/`user` jesli istnieje i jest nieuprzywilejowana;
///   2. inaczej rola NIEUPRZYWILEJOWANA o najmniejszej liczbie uprawnien;
///   3. gdy zadna nieuprzywilejowana nie istnieje — twardy blad (odmowa
///      czlonkostwa zamiast nadania admina).
fn pick_member_role(snapshot: &BaselineSnapshot) -> LedgerResult<String> {
    let non_privileged: Vec<&RoleRow> = snapshot
        .roles
        .iter()
        .filter(|r| !role_is_privileged(&r.permissions_json))
        .collect();

    if let Some(user_role) = non_privileged
        .iter()
        .find(|r| r.role_id == "role-user" || r.name == "user")
    {
        return Ok(user_role.role_id.clone());
    }

    non_privileged
        .iter()
        .min_by_key(|r| {
            serde_json::from_str::<Vec<String>>(&r.permissions_json)
                .map(|p| p.len())
                .unwrap_or(usize::MAX)
        })
        .map(|r| r.role_id.clone())
        .ok_or_else(|| {
            SyncLedgerError::Runtime(
                "baseline import: no non-privileged role available for a new member; refusing to \
                 grant a privileged role to a joiner user"
                    .into(),
            )
        })
}

/// Usuwa lokalne wiersze scope'owane org inna niz dawcy. Po wchlonieciu org
/// dawcy joiner nie ma juz wlasnej organizacji — wszystko nalezy do org dawcy.
fn drop_foreign_org_rows(
    tx: &Transaction<'_>,
    donor_org_id: &str,
    map_err: &impl Fn(rusqlite::Error) -> SyncLedgerError,
) -> LedgerResult<()> {
    tx.execute(
        "DELETE FROM org_memberships WHERE org_id <> ?1",
        params![donor_org_id],
    )
    .map_err(map_err)?;
    tx.execute(
        "DELETE FROM sync_user_org_profiles WHERE org_id <> ?1",
        params![donor_org_id],
    )
    .map_err(map_err)?;
    tx.execute(
        "DELETE FROM sync_resource_acl WHERE org_id <> ?1",
        params![donor_org_id],
    )
    .map_err(map_err)?;
    tx.execute(
        "DELETE FROM sync_policies WHERE org_id <> ?1",
        params![donor_org_id],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Krotki, stabilny suffix z UUID lokalnego rekordu — czyni rozsuwajacy suffix
/// kolizji deterministycznym (ten sam wejsciowy id daje ten sam suffix).
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

// =============================================================================
// Punkt wejscia dla transportu (krok 2 podpina iroh streaming)
// =============================================================================

/// Wykonuje pelna adopcje baseline'u po stronie joinera, majac juz surowe bajty
/// snapshotu dawcy (zlozone z chunkow). Krok 2 dostarcza `donor_snapshot_bytes`
/// ze streamu iroh (`BaselineHeader` + `BaselineChunk`*), woła te funkcje, po
/// czym odsyla `BaselineChunkAck`/zamyka. Tu logika importu jest KOMPLETNA i w
/// pelni testowalna in-process (testy wolaja ja z bajtami z `serialize_snapshot
/// + chunk_snapshot + reassemble_chunks`).
pub fn run_baseline_adopt(
    db: &DbPool,
    donor_node_id: &str,
    local_node_id: &str,
    donor_snapshot_bytes: &[u8],
    cipher: &crate::crypto::SettingsCipher,
) -> LedgerResult<BaselineImportReport> {
    let snapshot = deserialize_snapshot(donor_snapshot_bytes)?;
    import_baseline(db, &snapshot, donor_node_id, local_node_id, cipher)
}

#[cfg(test)]
mod tests;
