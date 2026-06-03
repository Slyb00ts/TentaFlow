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
use tracing::{info, warn};
use tentaflow_protocol::mesh::{BaselineAck, BaselineChunk, BaselineEpoch};

use crate::db::{self, DbPool};
use crate::sync::ledger::{LedgerResult, SyncLedgerError};

/// Klucz w `settings` trzymajacy persystowany single-flight stan adopcji.
/// Jeden wiersz na nod — adopcja jest globalna dla noda, nie per-peer, bo nod
/// moze byc w danym momencie albo dawca, albo joinerem, nigdy obojgiem.
pub const BASELINE_ADOPT_STATE_KEY: &str = "baseline_adopt_state";

/// Maksymalny rozmiar pojedynczego chunka baseline'u (bajty surowego CBOR).
/// Dobrany pod limit ramki iroh pairingu (64 KiB) z zapasem na naglowek CBOR
/// `BaselineChunk` (seq + 32-bajtowy hash + length-prefix bytes).
pub const BASELINE_CHUNK_BYTES: usize = 48 * 1024;

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
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))
}

/// Sprawdza, czy nod moze wejsc w `desired` role. Trwajaca adopcja w
/// przeciwnej roli (z faza != Completed) jest twardym bledem — to bramka
/// single-flight anty-split-brain. `Completed` w przeciwnej roli oznacza
/// zakonczona poprzednia adopcje i nie blokuje nowej.
pub fn guard_role(db: &DbPool, desired: BaselineRole) -> LedgerResult<()> {
    if let Some(existing) = load_adopt_state(db)? {
        if existing.role != desired && existing.phase != BaselinePhase::Completed {
            return Err(SyncLedgerError::Runtime(format!(
                "baseline adopt already in progress as {:?} with peer {} (phase {:?}); \
                 refusing to start as {:?}",
                existing.role, existing.peer, existing.phase, desired
            )));
        }
    }
    Ok(())
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

/// Buduje snapshot baseline'u z bazy dawcy w JEDNEJ transakcji read, dzieki
/// czemu wszystkie tabele widza spojny migawkowy stan (deferred-read snapshot
/// izolacji SQLite).
pub fn capture_baseline_snapshot(
    db: &DbPool,
    epoch: BaselineEpoch,
) -> LedgerResult<BaselineSnapshot> {
    let mut conn = db::repository::acquire_for_baseline(db)
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    let tx = conn
        .transaction()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;

    let snapshot = capture_baseline_snapshot_tx(&tx, epoch)?;
    // Read-only transakcja — commit zwalnia migawke bez zmian.
    tx.commit()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    Ok(snapshot)
}

fn capture_baseline_snapshot_tx(
    tx: &Transaction<'_>,
    epoch: BaselineEpoch,
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
                    published_model_name FROM flows",
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

/// Sklada chunki z powrotem w surowy snapshot, weryfikujac kolejnosc `seq` oraz
/// `content_hash` kazdego chunka. Bledny hash albo luka w sekwencji to twardy
/// blad — joiner nigdy nie importuje czesciowego/uszkodzonego baseline'u.
pub fn reassemble_chunks(chunks: &[BaselineChunk]) -> LedgerResult<Vec<u8>> {
    let mut ordered: Vec<&BaselineChunk> = chunks.iter().collect();
    ordered.sort_by_key(|c| c.seq);

    let mut out = Vec::new();
    for (expected_seq, chunk) in ordered.iter().enumerate() {
        if chunk.seq != expected_seq as u64 {
            return Err(SyncLedgerError::Runtime(format!(
                "baseline chunk sequence gap: expected seq {expected_seq}, got {}",
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
        out.extend_from_slice(&chunk.bytes);
    }
    Ok(out)
}

// =============================================================================
// Atomowy import (strona joinera)
// =============================================================================

/// Wynik importu — co dokladnie zostalo zmapowane/scalone. Uzywane przez UX
/// kroku 3 do pokazania podsumowania adopcji.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
) -> LedgerResult<BaselineImportReport> {
    let donor_org_id = primary_donor_org(snapshot)?;

    guard_role(db, BaselineRole::Joiner)?;
    if let Some(existing) = load_adopt_state(db)? {
        if existing.phase == BaselinePhase::Completed
            && existing.peer == donor_node_id
            && existing.epoch == snapshot.epoch
        {
            info!(
                donor = %donor_node_id,
                "baseline import already completed for this donor+epoch; skipping (idempotent re-pair)"
            );
            return Ok(BaselineImportReport {
                donor_org_id,
                ..Default::default()
            });
        }
    }

    store_adopt_state(
        db,
        &BaselineAdoptState {
            role: BaselineRole::Joiner,
            peer: donor_node_id.to_string(),
            epoch: snapshot.epoch.clone(),
            phase: BaselinePhase::Importing,
        },
    )?;

    // Caly import w jednej transakcji. Guard polaczenia jest scope'owany do tego
    // bloku, by zostal zwolniony PRZED ponownym `store_adopt_state` ponizej (std
    // Mutex nie jest reentrant — trzymanie guarda przy kolejnym `acquire`
    // zablokowaloby watek).
    let report = {
        let mut conn = db::repository::acquire_for_baseline(db)
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        let tx = conn
            .transaction()
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;

        let report = match import_baseline_tx(&tx, snapshot, &donor_org_id) {
            Ok(report) => report,
            Err(e) => {
                // Rollback nastapi automatycznie przy drop(tx); stan adopcji NIE
                // jest czyszczony tutaj — `Importing` przetrwa, a operator/krok 2
                // moze ponowic. Joiner pozostaje nietkniety.
                warn!(donor = %donor_node_id, "baseline import failed, rolling back: {e}");
                return Err(e);
            }
        };

        tx.commit()
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        report
    };

    // (d) Reset partycji core + adopt epoch dawcy + reseed. Wymaga aktywnego
    // runtime sync (Fjall ledger). W testach in-process (dwa goly DbPool bez
    // runtime) ten krok jest no-opem — sama transakcja SQLite jest juz
    // zatwierdzona i w pelni testowalna.
    if let Err(e) = crate::sync::runtime::adopt_donor_baseline_epoch(&snapshot.epoch) {
        warn!(donor = %donor_node_id, "baseline epoch adopt/reseed failed post-commit: {e}");
        return Err(e);
    }

    store_adopt_state(
        db,
        &BaselineAdoptState {
            role: BaselineRole::Joiner,
            peer: donor_node_id.to_string(),
            epoch: snapshot.epoch.clone(),
            phase: BaselinePhase::Completed,
        },
    )?;

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

/// Org dawcy, do ktorej joiner dolacza. Snapshot musi miec dokladnie jedna
/// nie-`deleted` organizacje (faza C laczy DWA single-org nody); inaczej elekcja
/// org jest niejednoznaczna i import jest odrzucany.
fn primary_donor_org(snapshot: &BaselineSnapshot) -> LedgerResult<String> {
    let mut active: Vec<&OrganizationRow> = snapshot
        .organizations
        .iter()
        .filter(|o| o.status != "deleted")
        .collect();
    active.sort_by(|a, b| a.org_id.cmp(&b.org_id));
    match active.as_slice() {
        [only] => Ok(only.org_id.clone()),
        [] => Err(SyncLedgerError::Runtime(
            "baseline snapshot carries no active organization".into(),
        )),
        many => {
            // Deterministyczny wybor: org o najnizszym org_id. Logujemy, bo to
            // sygnal, ze dawca byl juz multi-org (nieoczekiwane w fazie C).
            warn!(
                "baseline snapshot carries {} active orgs; adopting lowest org_id",
                many.len()
            );
            Ok(many[0].org_id.clone())
        }
    }
}

fn import_baseline_tx(
    tx: &Transaction<'_>,
    snapshot: &BaselineSnapshot,
    donor_org_id: &str,
) -> LedgerResult<BaselineImportReport> {
    let map_err = |e: rusqlite::Error| SyncLedgerError::Runtime(e.to_string());
    let mut report = BaselineImportReport {
        donor_org_id: donor_org_id.to_string(),
        ..Default::default()
    };

    // Zbior emaili joinera -> id usera dawcy, do mapowania tozsamosci ludzi.
    // Match po DOKLADNYM (case-sensitive, przycietym) emailu.
    let donor_email_to_id: BTreeMap<String, String> = snapshot
        .user_accounts
        .iter()
        .filter_map(|u| {
            u.email
                .as_deref()
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(|e| (e.to_string(), u.id.clone()))
        })
        .collect();
    let donor_ids: std::collections::BTreeSet<&str> =
        snapshot.user_accounts.iter().map(|u| u.id.as_str()).collect();

    // Lokalni (joinera) userzy PRZED importem — uzywane do remapu i kolizji.
    let local_users = read_local_users(tx)?;

    // (b) Kolizje UNIQUE: dawca wygrywa. Najpierw rozsuwamy kolidujace UNIQUE
    // wartosci po stronie joinera (zanim wstawimy wiersze dawcy), aby INSERT
    // dawcy nie wpadl na istniejacy lokalny rekord o tej samej wartosci.
    suffix_local_collisions(tx, snapshot, &mut report, &map_err)?;

    // (a) Upsert wierszy dawcy po UUID PK. Deterministyczne seedy (np.
    // role-org-admin, org-default) zlewaja sie po tym samym id; user-created
    // dawcy sa wstawiane jako nowe.
    upsert_donor_rows(tx, snapshot, &map_err)?;

    // (c) Remap lokalnych danych joinera do org dawcy. Email-match mapuje na
    // usera dawcy; inaczej user joinera dolacza jako nowy czlonek org dawcy.
    for local in &local_users {
        let local_email = local.email.as_deref().map(str::trim).filter(|e| !e.is_empty());
        let mapped_donor_id = local_email.and_then(|e| donor_email_to_id.get(e));

        if let Some(donor_id) = mapped_donor_id {
            if donor_id.as_str() != local.id {
                report.users_merged_by_email += 1;
            }
            // User joinera jest TYM SAMYM czlowiekiem co user dawcy — przepinamy
            // dane joinera na id dawcy, lokalny wiersz znika (chyba ze to sam
            // dawca, wtedy no-op).
            remap_user_owned_rows(tx, &local.id, donor_id, donor_org_id, &map_err)?;
        } else if !donor_ids.contains(local.id.as_str()) {
            // Nowy czlowiek — zostaje wlasnym userem, ale staje sie czlonkiem
            // org dawcy. Jego dane owner/assigned pozostaja jego, tylko org_id
            // jest przepinany na org dawcy.
            report.users_joined_donor_org += 1;
            attach_local_user_to_donor_org(tx, &local.id, donor_org_id, snapshot, &map_err)?;
        }
    }

    // Po remapie usuwamy lokalne org_memberships/profile wskazujace na orgi inne
    // niz dawcy (joiner nie ma juz wlasnej org — wchlonal org dawcy).
    drop_foreign_org_rows(tx, donor_org_id, &map_err)?;

    Ok(report)
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

/// (b) Rozsuwa kolidujace UNIQUE wartosci joinera, gdy dawca niesie rekord o
/// innym UUID PK ale tej samej wartosci UNIQUE. Dawca wygrywa: lokalny rekord
/// joinera dostaje deterministyczny suffix (`<value>-<short_local_id>`), a
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
            let suffixed = format!("{}-{}", donor.username, short_id(&local_id));
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
            let suffixed = format!("{}-{}", donor.slug, short_id(&local_id));
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
            let suffixed = format!("{}-{}", donor.name, short_id(&local_id));
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
            let suffixed = format!("{}-{}", donor.name, short_id(&local_id));
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
            let suffixed = format!("{}-{}", donor.model_pattern, short_id(&local_id));
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

/// (a) Wstawia/aktualizuje wiersze dawcy po UUID PK. INSERT ... ON CONFLICT(PK)
/// DO UPDATE — deterministyczne seedy (te same UUID) sa scalane, user-created
/// dawcy wstawiane. Kolejnosc respektuje FK: organizacje/role -> user_accounts
/// -> grupy -> czlonkostwa -> flows -> bindings -> sync_*.
fn upsert_donor_rows(
    tx: &Transaction<'_>,
    snapshot: &BaselineSnapshot,
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
                 published_model_name) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(id) DO UPDATE SET \
                name = excluded.name, description = excluded.description, \
                is_default = excluded.is_default, service_type = excluded.service_type, \
                flow_json = excluded.flow_json, status = excluded.status, \
                published_model_name = excluded.published_model_name",
            params![
                f.id,
                f.name,
                f.description,
                f.is_default,
                f.service_type,
                f.flow_json,
                f.status,
                f.published_model_name
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

    // org_memberships joinera dla tego usera znikna z `drop_foreign_org_rows`;
    // usuwamy lokalny wiersz usera joinera (dane juz przepiete na dawce).
    tx.execute(
        "DELETE FROM user_accounts WHERE id = ?1",
        params![local_id],
    )
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
    let role_id = pick_member_role(snapshot);
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

/// Wybiera role dla nowego czlonka org dawcy. Preferuje `role-user` (najmniej
/// uprzywilejowana w domyslnym seedzie); fallback do pierwszej dostepnej roli.
fn pick_member_role(snapshot: &BaselineSnapshot) -> String {
    snapshot
        .roles
        .iter()
        .find(|r| r.role_id == "role-user" || r.name == "user")
        .or_else(|| snapshot.roles.first())
        .map(|r| r.role_id.clone())
        .unwrap_or_else(|| "role-user".to_string())
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
    donor_snapshot_bytes: &[u8],
) -> LedgerResult<BaselineImportReport> {
    let snapshot = deserialize_snapshot(donor_snapshot_bytes)?;
    import_baseline(db, &snapshot, donor_node_id)
}

#[cfg(test)]
mod tests;
