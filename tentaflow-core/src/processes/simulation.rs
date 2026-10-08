// ============ File: simulation.rs — isolated deterministic BPMN simulation store ============

use std::cell::RefCell;
use std::collections::{hash_map::Entry, BTreeMap, HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use std::sync::{Arc, Mutex, Weak};

use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tentaflow_protocol::processes::{
    ProcessIncident, ProcessInstance, ProcessInstanceStatus, ProcessModel, ProcessNodeKind,
    ProcessTimerSpec, ProcessTimerStatus, ProcessUserTask, ProcessUserTaskKind,
    ProcessTimerSummary,
};
use uuid::Uuid;

use super::model::{selected_body, validate_model, validate_variables};
use super::repository::{
    self, AcceptedInputRef, ActivityIoInputFact, ActivityIoWitness, DueTimer, GatewayReceipt,
    ProcessActor, ProcessToken, RuntimePlan, RuntimeSnapshot, StartInputRef, TimerSnapshot,
};
use super::runtime::{
    plan_advance_with_id_source, plan_manual_acknowledgment_with_id_source,
    plan_start_with_id_source, plan_user_completion_with_id_source, RuntimeIdSource,
    RuntimeIdSourceHandle, StartCause,
};

use super::simulation_schema;
use crate::db::DbPool;

const PROFILE_NAME: &str = "script_user_manual";
const MAX_ACL_BYTES: usize = 256 * 1024;
const MAX_JSON_BYTES: usize = 256 * 1024;
const MAX_TRACE_BYTES: usize = 4 * 1024 * 1024;
// A private run is bounded by measured SQLite pages as well as the existing
// model, variable, task, and trace payload limits. The registry reserves the
// same finite envelope for every retained run, so one actor cannot pin the
// process with an unbounded number of detached databases.
const MAX_SIMULATION_DATABASE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SIMULATION_REGISTRY_RUNS: usize = 16;
const MAX_SIMULATION_RUNS_PER_OWNER: usize = 4;
const MAX_SIMULATION_REGISTRY_BYTES: u64 =
    MAX_SIMULATION_DATABASE_BYTES * MAX_SIMULATION_REGISTRY_RUNS as u64;

#[cfg(test)]
thread_local! {
    static SIMULATION_TRANSITION_PREFLIGHT:
        RefCell<Option<Box<dyn FnOnce(&Connection, &str) -> Result<()>>>> =
        RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn set_simulation_transition_preflight(
    hook: impl FnOnce(&Connection, &str) -> Result<()> + 'static,
) -> Result<()> {
    SIMULATION_TRANSITION_PREFLIGHT.with(|slot| {
        let mut slot = slot.borrow_mut();
        ensure!(slot.is_none(), "simulation transition preflight is already installed");
        *slot = Some(Box::new(hook));
        Ok(())
    })
}

#[cfg(test)]
fn run_simulation_transition_preflight(
    connection: &Connection,
    command_id: &str,
) -> Result<()> {
    SIMULATION_TRANSITION_PREFLIGHT.with(|slot| {
        let hook = slot.borrow_mut().take();
        hook.map(|hook| hook(connection, command_id)).transpose()?;
        Ok(())
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationAclSnapshot {
    pub permitted_user_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SimulationSourceInput {
    pub org_id: String,
    pub owner_user_id: String,
    pub actor_user_id: String,
    pub definition_id: String,
    pub version: u32,
    pub model_json: String,
    pub model_sha256: String,
    pub selected_process_id: String,
    pub start_node_id: String,
    pub acl_snapshot_json: String,
    pub scenario_sha256: String,
    pub start_ms: i64,
    pub horizon_ms: i64,
    pub tick_duration_ms: i64,
}

/// A source snapshot that was captured by the authenticated process repository.
///
/// The fields deliberately stay private. A simulation must never treat a model,
/// hash, or ACL supplied by a transport client as proof of publication or access.
pub(crate) struct AuthenticatedSimulationSource {
    input: SimulationSourceInput,
}

impl AuthenticatedSimulationSource {
    #[cfg(test)]
    pub(crate) fn from_test(input: SimulationSourceInput) -> Self {
        Self { input }
    }

    pub(crate) fn input(&self) -> &SimulationSourceInput {
        &self.input
    }
}

/// A short-lived authorization minted from the live process repository for one
/// simulator read or transition. The store never retains the production pool;
/// callers must obtain a new capability after every user-visible operation.
pub(crate) struct AuthenticatedSimulationAction {
    simulation_id: String,
    org_id: String,
    actor_user_id: String,
    definition_id: String,
    version: u32,
    model_sha256: String,
}

impl AuthenticatedSimulationAction {
    pub(super) fn from_live(
        simulation_id: &str,
        actor: &ProcessActor,
        definition_id: &str,
        version: u32,
        model_sha256: &str,
    ) -> Self {
        Self {
            simulation_id: simulation_id.to_owned(),
            org_id: actor.org_id.clone(),
            actor_user_id: actor.user_id.clone(),
            definition_id: definition_id.to_owned(),
            version,
            model_sha256: model_sha256.to_owned(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(source: &SimulationSourcePin, actor: &ProcessActor) -> Self {
        Self::from_live(
            &source.simulation_id,
            actor,
            &source.definition_id,
            source.version,
            &source.model_sha256,
        )
    }

    fn verify(&self, source: &SimulationSourcePin, actor: &ProcessActor) -> Result<()> {
        ensure!(
            self.simulation_id == source.simulation_id
                && self.org_id == source.org_id
                && self.actor_user_id == actor.user_id
                && self.org_id == actor.org_id
                && self.definition_id == source.definition_id
                && self.version == source.version
                && self.model_sha256 == source.model_sha256,
            "simulation action authorization does not match its pinned source"
        );
        Ok(())
    }
}

pub(crate) fn capture_authenticated_source(
    actor: &ProcessActor,
    owner_user_id: &str,
    definition_id: &str,
    version: u32,
    model: &ProcessModel,
    model_sha256: &str,
    selected_process_id: &str,
    start_node_id: &str,
    permitted_user_ids: &[String],
    variables: &Value,
    start_ms: i64,
    horizon_ms: i64,
    tick_duration_ms: i64,
) -> Result<AuthenticatedSimulationSource> {
    let model_json = serde_json::to_string(model).context("encode published simulation model")?;
    let scenario_sha256 = simulation_scenario_sha256(
        definition_id,
        version,
        model_sha256,
        selected_process_id,
        start_node_id,
        variables,
        start_ms,
        horizon_ms,
        tick_duration_ms,
    )?;
    let acl_snapshot_json = serde_json::to_string(&SimulationAclSnapshot {
        permitted_user_ids: permitted_user_ids.to_vec(),
    })?;
    let input = SimulationSourceInput {
        org_id: actor.org_id.clone(),
        owner_user_id: owner_user_id.to_owned(),
        actor_user_id: actor.user_id.clone(),
        definition_id: definition_id.to_owned(),
        version,
        model_json,
        model_sha256: model_sha256.to_owned(),
        selected_process_id: selected_process_id.to_owned(),
        start_node_id: start_node_id.to_owned(),
        acl_snapshot_json,
        scenario_sha256,
        start_ms,
        horizon_ms,
        tick_duration_ms,
    };
    validate_captured_source(&input)?;
    Ok(AuthenticatedSimulationSource { input })
}

/// Computes the logical scenario identity from the source that the repository
/// authenticated and the exact initial request accepted for the run. The
/// transport never supplies this digest; it is only used for deterministic
/// child identifiers inside one detached run.
pub(crate) fn simulation_scenario_sha256(
    definition_id: &str,
    version: u32,
    model_sha256: &str,
    selected_process_id: &str,
    start_node_id: &str,
    variables: &Value,
    start_ms: i64,
    horizon_ms: i64,
    tick_duration_ms: i64,
) -> Result<String> {
    validate_sha256(model_sha256, "published model SHA-256")?;
    validate_variables(variables)?;
    let scenario = json!({
        "schema": "tentaflow.simulation.scenario.v2",
        "definition_id": definition_id,
        "version": version,
        "model_sha256": model_sha256,
        "selected_process_id": selected_process_id,
        "start_node_id": start_node_id,
        "variables": variables,
        "clock": {
            "start_ms": start_ms,
            "horizon_ms": horizon_ms,
            "tick_duration_ms": tick_duration_ms,
        },
    });
    let mut hash = Sha256::new();
    hash.update(b"tentaflow:simulation-scenario:v2:");
    hash.update(serde_json::to_vec(&repository::canonical_json_value(&scenario))?);
    Ok(hex::encode(hash.finalize()))
}

/// Owns the disposable database used by one simulation.
///
/// Keeping connection construction here prevents a caller from handing the
/// simulator a production connection and causing `simulation_*` tables to be
/// created in the application database.
#[derive(Debug)]
pub struct SimulationDatabase {
    conn: Connection,
}

fn connection_resident_bytes(conn: &Connection) -> Result<u64> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let page_count =
        u64::try_from(page_count).context("private simulation page count is negative")?;
    let page_size = u64::try_from(page_size).context("private simulation page size is negative")?;
    page_count
        .checked_mul(page_size)
        .context("private simulation resident size overflow")
}

impl SimulationDatabase {
    pub(crate) fn open() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open disposable simulation database")?;
        crate::db::migrations::run(&conn)
            .context("initialize the private production process schema")?;
        simulation_schema::initialize(&conn)?;
        Ok(Self { conn })
    }

    fn into_connection(self) -> Connection {
        self.conn
    }

    fn resident_bytes(&self) -> Result<u64> {
        connection_resident_bytes(&self.conn)
    }

    #[cfg(test)]
    pub(crate) fn from_connection_for_test(conn: Connection) -> Self {
        Self { conn }
    }

    #[cfg(test)]
    pub(crate) fn into_connection_for_test(self) -> Connection {
        self.conn
    }
}

/// Holds private simulation databases without retaining a production pool or
/// connection. A request takes one database for the duration of its operation
/// and returns it before the response is sent.
#[derive(Debug)]
struct SimulationRegistryEntry {
    database: Option<SimulationDatabase>,
    org_id: String,
    owner_user_id: String,
    resident_bytes: u64,
    release_requested: bool,
}

#[derive(Debug, Default)]
struct SimulationRegistryState {
    databases: HashMap<String, SimulationRegistryEntry>,
    reservations: HashMap<String, SimulationStartReservationEntry>,
}

#[derive(Debug)]
struct SimulationStartReservationEntry {
    owner_user_id: String,
}

/// Counts one pending Start before it constructs its private SQLite database.
/// Dropping an uncommitted reservation releases its slot on every error path.
#[derive(Debug)]
pub(crate) struct SimulationStartReservation {
    token: String,
    owner_user_id: String,
    state: Weak<Mutex<SimulationRegistryState>>,
    active: bool,
}

impl Drop for SimulationStartReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(state) = self.state.upgrade() {
            if let Ok(mut state) = state.lock() {
                state.reservations.remove(&self.token);
            }
        }
    }
}

#[derive(Debug)]
enum ReleaseTarget {
    Available(SimulationDatabase),
    InFlight {
        org_id: String,
        owner_user_id: String,
    },
}

#[derive(Debug, Default)]
pub struct SimulationRegistry {
    state: Arc<Mutex<SimulationRegistryState>>,
}

impl SimulationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserves one complete per-run envelope before opening SQLite or
    /// materializing the production schema. The reservation is released by
    /// Drop unless the caller commits the finished database.
    pub(crate) fn reserve_start(
        &self,
        owner_user_id: &str,
    ) -> Result<SimulationStartReservation> {
        self.reserve_start_with_token(Uuid::new_v4().to_string(), owner_user_id)
    }

    fn reserve_start_with_token(
        &self,
        token: String,
        owner_user_id: &str,
    ) -> Result<SimulationStartReservation> {
        ensure!(!owner_user_id.is_empty(), "simulation owner is empty");
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("simulation registry lock is poisoned"))?;
        ensure!(
            state.databases.len().saturating_add(state.reservations.len())
                < MAX_SIMULATION_REGISTRY_RUNS,
            "simulation registry has reached its active run limit"
        );
        let owner_count = registry_owner_count(&state, owner_user_id);
        ensure!(
            owner_count < MAX_SIMULATION_RUNS_PER_OWNER,
            "simulation owner has reached the active run limit"
        );
        let resident_total = registry_resident_bytes(&state.databases)?;
        let reserved_total = registry_reserved_bytes(&state.reservations)?;
        ensure!(
            resident_total
                .checked_add(reserved_total)
                .and_then(|total| total.checked_add(MAX_SIMULATION_DATABASE_BYTES))
                .context("simulation registry resident size overflow")?
                <= MAX_SIMULATION_REGISTRY_BYTES,
            "simulation registry has reached its resident size budget"
        );
        match state.reservations.entry(token.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(SimulationStartReservationEntry {
                    owner_user_id: owner_user_id.to_owned(),
                });
            }
            Entry::Occupied(_) => bail!("simulation start reservation collided"),
        }
        Ok(SimulationStartReservation {
            token,
            owner_user_id: owner_user_id.to_owned(),
            state: Arc::downgrade(&self.state),
            active: true,
        })
    }

    #[cfg(test)]
    pub(crate) fn insert(&self, simulation_id: String, database: SimulationDatabase) -> Result<()> {
        let source = SimulationStore::source_pin(&database, &simulation_id)?;
        let resident_bytes = database.resident_bytes()?;
        ensure!(
            resident_bytes <= MAX_SIMULATION_DATABASE_BYTES,
            "simulation private database exceeds its resident size budget"
        );
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("simulation registry lock is poisoned"))?;
        if state.databases.contains_key(&simulation_id) {
            bail!("simulation identifier is already registered: {simulation_id}");
        }
        ensure!(
            state.databases.len().saturating_add(state.reservations.len())
                < MAX_SIMULATION_REGISTRY_RUNS,
            "simulation registry has reached its active run limit"
        );
        let owner_count = registry_owner_count(&state, &source.owner_user_id);
        ensure!(
            owner_count < MAX_SIMULATION_RUNS_PER_OWNER,
            "simulation owner has reached the active run limit"
        );
        let resident_total = registry_resident_bytes(&state.databases)?;
        let reserved_total = registry_reserved_bytes(&state.reservations)?;
        ensure!(
            resident_total
                .checked_add(reserved_total)
                .and_then(|total| total.checked_add(resident_bytes))
                .context("simulation registry resident size overflow")?
                <= MAX_SIMULATION_REGISTRY_BYTES,
            "simulation registry has reached its resident size budget"
        );
        match state.databases.entry(simulation_id) {
            Entry::Vacant(entry) => {
                entry.insert(SimulationRegistryEntry {
                    database: Some(database),
                    org_id: source.org_id,
                    owner_user_id: source.owner_user_id,
                    resident_bytes,
                    release_requested: false,
                });
            }
            Entry::Occupied(_) => unreachable!("simulation registry entry was checked above"),
        }
        Ok(())
    }

    pub(crate) fn take(&self, simulation_id: &str) -> Result<SimulationDatabase> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("simulation registry lock is poisoned"))?;
        let entry = state
            .databases
            .get_mut(simulation_id)
            .context("simulation not found on this node")?;
        entry
            .database
            .take()
            .context("simulation operation is already in progress")
    }

    pub(crate) fn put(&self, simulation_id: String, database: SimulationDatabase) -> Result<()> {
        let source = SimulationStore::source_pin(&database, &simulation_id)?;
        let resident_bytes = database.resident_bytes()?;
        ensure!(
            resident_bytes <= MAX_SIMULATION_DATABASE_BYTES,
            "simulation private database exceeds its resident size budget"
        );
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("simulation registry lock is poisoned"))?;
        let release_requested = {
            let entry = state
                .databases
                .get_mut(&simulation_id)
            .with_context(|| format!("simulation identifier was removed concurrently: {simulation_id}"))?;
            ensure!(
                entry.database.is_none(),
                "simulation operation was registered concurrently"
            );
            ensure!(
                entry.org_id == source.org_id && entry.owner_user_id == source.owner_user_id,
                "simulation owner changed in its private source pin"
            );
            entry.release_requested
        };
        if release_requested {
            state.databases.remove(&simulation_id);
            drop(state);
            drop(database);
            return Ok(());
        }
        let entry = state
            .databases
            .get_mut(&simulation_id)
            .context("simulation identifier was removed concurrently")?;
        entry.database = Some(database);
        entry.resident_bytes = resident_bytes;
        Ok(())
    }

    fn take_for_release(&self, simulation_id: &str) -> Result<ReleaseTarget> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("simulation registry lock is poisoned"))?;
        let entry = state
            .databases
            .get_mut(simulation_id)
            .context("simulation not found on this node")?;
        if let Some(database) = entry.database.take() {
            return Ok(ReleaseTarget::Available(database));
        }
        Ok(ReleaseTarget::InFlight {
            org_id: entry.org_id.clone(),
            owner_user_id: entry.owner_user_id.clone(),
        })
    }

    pub(crate) fn mark_release_requested(&self, simulation_id: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("simulation registry lock is poisoned"))?;
        let remove_now = match state.databases.get_mut(simulation_id) {
            Some(entry) if entry.database.is_some() => true,
            Some(entry) => {
                entry.release_requested = true;
                false
            }
            None => return Ok(()),
        };
        if remove_now {
            state.databases.remove(simulation_id);
        }
        Ok(())
    }

    fn remove_released(&self, simulation_id: &str, database: SimulationDatabase) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("simulation registry lock is poisoned"))?;
        let entry = state
            .databases
            .get(simulation_id)
            .context("simulation release entry disappeared concurrently")?;
        ensure!(
            entry.database.is_none(),
            "simulation release raced with a new operation"
        );
        let entry = state
            .databases
            .remove(simulation_id)
            .context("simulation release entry disappeared concurrently")?;
        debug_assert!(entry.database.is_none());
        drop(state);
        drop(database);
        Ok(())
    }

    /// Permanently drops one private run after the retained owner's live
    /// identity check. Cleanup remains possible after publication archival or
    /// revocation, but this path never returns a transition capability.
    pub(crate) fn release(
        &self,
        pool: &DbPool,
        actor: &ProcessActor,
        simulation_id: &str,
    ) -> Result<()> {
        match self.take_for_release(simulation_id)? {
            ReleaseTarget::Available(database) => {
                let source = match SimulationStore::source_pin(&database, simulation_id) {
                    Ok(source) => source,
                    Err(error) => {
                        self.put(simulation_id.to_owned(), database)?;
                        return Err(error);
                    }
                };
                if let Err(error) = repository::authorize_simulation_release(
                    pool,
                    actor,
                    &source.org_id,
                    &source.owner_user_id,
                ) {
                    self.put(simulation_id.to_owned(), database)?;
                    return Err(error);
                }
                self.remove_released(simulation_id, database)
            }
            ReleaseTarget::InFlight {
                org_id,
                owner_user_id,
            } => {
                repository::authorize_simulation_release(
                    pool,
                    actor,
                    &org_id,
                    &owner_user_id,
                )?;
                self.mark_release_requested(simulation_id)
            }
        }
    }

    /// Re-authenticates against the live process repository for every
    /// operation, then runs one owned store operation with a one-shot action.
    pub(crate) fn with_authorized_store<T, F>(
        &self,
        pool: &DbPool,
        actor: &ProcessActor,
        simulation_id: &str,
        operation: F,
    ) -> Result<T>
    where
        F: FnOnce(&mut SimulationStore, AuthenticatedSimulationAction) -> Result<T>,
    {
        let database = self.take(simulation_id)?;
        let source = match SimulationStore::source_pin(&database, simulation_id) {
            Ok(source) => source,
            Err(error) => {
                self.put(simulation_id.to_owned(), database)?;
                return Err(error);
            }
        };
        let authorization = match repository::authorize_simulation_action(pool, actor, &source) {
            Ok(authorization) => authorization,
            Err(error) => {
                self.put(simulation_id.to_owned(), database)?;
                return Err(error);
            }
        };
        let mut store =
            match SimulationStore::open(database, simulation_id, actor.clone(), authorization) {
                Ok(store) => store,
                Err(error) => {
                    self.put(simulation_id.to_owned(), error.database)?;
                    return Err(error.error);
                }
            };
        let operation_authorization =
            match repository::authorize_simulation_action(pool, actor, &source) {
                Ok(authorization) => authorization,
                Err(error) => {
                    let database = store.into_database();
                    self.put(simulation_id.to_owned(), database)?;
                    return Err(error);
                }
            };
        let result = operation(&mut store, operation_authorization);
        let database = store.into_database();
        self.put(simulation_id.to_owned(), database)?;
        result
    }
}

impl SimulationStartReservation {
    pub(crate) fn commit(
        mut self,
        simulation_id: String,
        database: SimulationDatabase,
    ) -> Result<()> {
        let source = SimulationStore::source_pin(&database, &simulation_id)?;
        let resident_bytes = database.resident_bytes()?;
        ensure!(
            resident_bytes <= MAX_SIMULATION_DATABASE_BYTES,
            "simulation private database exceeds its resident size budget"
        );
        ensure!(
            source.owner_user_id == self.owner_user_id,
            "simulation owner changed before Start reservation commit"
        );
        let state = self
            .state
            .upgrade()
            .context("simulation registry was dropped before Start commit")?;
        let mut state = state
            .lock()
            .map_err(|_| anyhow::anyhow!("simulation registry lock is poisoned"))?;
        let reservation = state
            .reservations
            .get(&self.token)
            .context("simulation Start reservation is no longer active")?;
        ensure!(
            reservation.owner_user_id == self.owner_user_id,
            "simulation Start reservation owner changed"
        );
        ensure!(
            state.databases.len().saturating_add(state.reservations.len())
                <= MAX_SIMULATION_REGISTRY_RUNS,
            "simulation registry has reached its active run limit"
        );
        ensure!(
            registry_owner_count(&state, &self.owner_user_id)
                <= MAX_SIMULATION_RUNS_PER_OWNER,
            "simulation owner has reached the active run limit"
        );
        let reserved_total = registry_reserved_bytes(&state.reservations)?;
        let other_reserved = reserved_total
            .checked_sub(MAX_SIMULATION_DATABASE_BYTES)
            .context("simulation Start reservation accounting underflow")?;
        let resident_total = registry_resident_bytes(&state.databases)?;
        ensure!(
            resident_total
                .checked_add(other_reserved)
                .and_then(|total| total.checked_add(resident_bytes))
                .context("simulation registry resident size overflow")?
                <= MAX_SIMULATION_REGISTRY_BYTES,
            "simulation registry has reached its resident size budget"
        );
        ensure!(
            !state.databases.contains_key(&simulation_id),
            "simulation identifier is already registered: {simulation_id}"
        );
        state.reservations.remove(&self.token);
        state.databases.insert(
            simulation_id,
            SimulationRegistryEntry {
                database: Some(database),
                org_id: source.org_id,
                owner_user_id: source.owner_user_id,
                resident_bytes,
                release_requested: false,
            },
        );
        self.active = false;
        Ok(())
    }
}

fn registry_resident_bytes(
    databases: &HashMap<String, SimulationRegistryEntry>,
) -> Result<u64> {
    databases.values().try_fold(0_u64, |total, entry| {
        total
            .checked_add(entry.resident_bytes)
            .context("simulation registry resident size overflow")
    })
}

fn registry_reserved_bytes(
    reservations: &HashMap<String, SimulationStartReservationEntry>,
) -> Result<u64> {
    u64::try_from(reservations.len())
        .context("simulation reservation count does not fit in resident-size accounting")?
        .checked_mul(MAX_SIMULATION_DATABASE_BYTES)
        .context("simulation registry reserved size overflow")
}

fn registry_owner_count(state: &SimulationRegistryState, owner_user_id: &str) -> usize {
    state
        .databases
        .values()
        .filter(|entry| entry.owner_user_id == owner_user_id)
        .count()
        .saturating_add(
            state
                .reservations
                .values()
                .filter(|reservation| reservation.owner_user_id == owner_user_id)
                .count(),
        )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationSourcePin {
    pub simulation_id: String,
    pub org_id: String,
    pub owner_user_id: String,
    pub actor_user_id: String,
    pub definition_id: String,
    pub version: u32,
    pub model_sha256: String,
    pub model_json: String,
    pub selected_process_id: String,
    pub start_node_id: String,
    pub acl_snapshot_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SimulationClock {
    pub start_ms: i64,
    pub now_ms: i64,
    pub horizon_ms: i64,
    pub tick_duration_ms: i64,
    pub step_index: u64,
    pub revision: u64,
}

impl SimulationClock {
    pub fn new(start_ms: i64, horizon_ms: i64, tick_duration_ms: i64) -> Result<Self> {
        ensure!(
            horizon_ms >= start_ms,
            "simulation horizon precedes its start"
        );
        ensure!(
            tick_duration_ms > 0,
            "simulation tick duration must be positive"
        );
        Ok(Self {
            start_ms,
            now_ms: start_ms,
            horizon_ms,
            tick_duration_ms,
            step_index: 0,
            revision: 1,
        })
    }

    pub fn next_time(&self) -> Result<i64> {
        let now = self
            .now_ms
            .checked_add(self.tick_duration_ms)
            .context("simulation virtual clock overflow")?;
        ensure!(
            now <= self.horizon_ms,
            "simulation virtual clock exceeded its horizon"
        );
        Ok(now)
    }

    pub fn accept_transition(&mut self, now_ms: i64) -> Result<()> {
        let expected_now = if self.step_index == 0 {
            self.now_ms
        } else {
            self.now_ms
                .checked_add(self.tick_duration_ms)
                .context("simulation virtual clock overflow")?
        };
        ensure!(
            now_ms == expected_now,
            "simulation transition does not match the configured virtual tick"
        );
        ensure!(
            now_ms <= self.horizon_ms,
            "simulation clock exceeded its horizon"
        );
        self.step_index = self
            .step_index
            .checked_add(1)
            .context("simulation logical step overflow")?;
        self.revision = self
            .revision
            .checked_add(1)
            .context("simulation clock revision overflow")?;
        self.now_ms = now_ms;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SimulationIdSource {
    scenario_sha256: String,
    step_index: u64,
    ordinals: BTreeMap<String, u64>,
    error: Option<String>,
    issued: HashSet<String>,
}

impl SimulationIdSource {
    pub fn new(scenario_sha256: &str) -> Result<Self> {
        validate_sha256(scenario_sha256, "scenario SHA-256")?;
        Ok(Self {
            scenario_sha256: scenario_sha256.to_owned(),
            step_index: 0,
            ordinals: BTreeMap::new(),
            error: None,
            issued: HashSet::new(),
        })
    }

    pub fn set_step_index(&mut self, step_index: u64) {
        self.step_index = step_index;
        self.ordinals.clear();
        self.error = None;
    }

    pub fn issued(&self) -> &HashSet<String> {
        &self.issued
    }

    fn next_checked(&mut self, kind: &str) -> Option<String> {
        if self.error.is_some() {
            return None;
        }
        if kind.is_empty() || kind.contains('/') {
            self.error = Some(format!("invalid simulation identifier kind: {kind:?}"));
            return None;
        }
        let ordinal = self.ordinals.get(kind).copied().unwrap_or(0);
        let Some(next_ordinal) = ordinal.checked_add(1) else {
            self.error = Some("simulation identifier ordinal overflow".into());
            return None;
        };
        self.ordinals.insert(kind.to_owned(), next_ordinal);
        let name = format!(
            "tentaflow.simulation.v1/{}/{}/{}/{}",
            self.scenario_sha256, kind, self.step_index, ordinal
        );
        let id = Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes()).to_string();
        if Uuid::parse_str(&id).is_err() {
            self.error = Some("UUID-v5 simulation identifier was not parseable".into());
            return None;
        }
        self.issued.insert(id.clone());
        Some(id)
    }
}

impl RuntimeIdSource for SimulationIdSource {
    fn next_id(&mut self, kind: &str) -> String {
        self.next_checked(kind)
            .unwrap_or_else(|| Uuid::nil().to_string())
    }

    fn error(&self) -> Option<String> {
        self.error.clone()
    }

    fn accepts(&self, id: &str) -> bool {
        self.issued.contains(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimulationUnsupportedActivity {
    ServiceTask { node_id: String },
    CallActivity { node_id: String },
    SubProcess { node_id: String },
    ExternalActivity { node_id: String, kind: String },
    Repetition { node_id: String },
    AdditionalProcess,
}

impl fmt::Display for SimulationUnsupportedActivity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServiceTask { node_id } => {
                write!(f, "ServiceTask is unsupported in simulation ({node_id})")
            }
            Self::CallActivity { node_id } => {
                write!(f, "CallActivity is unsupported in simulation ({node_id})")
            }
            Self::SubProcess { node_id } => {
                write!(f, "SubProcess is unsupported in simulation ({node_id})")
            }
            Self::ExternalActivity { node_id, kind } => {
                write!(f, "{kind} is unsupported in simulation ({node_id})")
            }
            Self::Repetition { node_id } => {
                write!(f, "repetition is unsupported in simulation ({node_id})")
            }
            Self::AdditionalProcess => {
                write!(f, "additional process bodies are unsupported in simulation")
            }
        }
    }
}

impl std::error::Error for SimulationUnsupportedActivity {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationView {
    pub simulation_id: String,
    pub source: SimulationSourcePin,
    pub clock: SimulationClock,
    pub instance: Option<ProcessInstance>,
    pub user_tasks: Vec<ProcessUserTask>,
    pub timers: Vec<ProcessTimerSummary>,
    pub tokens: Vec<ProcessToken>,
    pub gateway_receipts: Vec<GatewayReceipt>,
    pub incidents: Vec<ProcessIncident>,
    pub events: Vec<SimulationEventView>,
    pub trace_steps: Vec<SimulationTraceStep>,
    pub activity_io_witnesses: Vec<ActivityIoWitness>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulationEventView {
    pub event_id: String,
    pub seq: u64,
    pub at_ms: i64,
    pub kind: String,
    pub node_id: Option<String>,
    pub actor_user_id: Option<String>,
    pub data: Value,
    pub scope_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationTraceStep {
    pub trace_step_id: String,
    pub ordinal: u64,
    pub action: String,
    pub at_ms: i64,
    pub request_sha256: String,
    pub result_sha256: String,
    pub data: Value,
}

#[derive(Debug)]
pub struct SimulationStore {
    conn: Connection,
    simulation_id: String,
    source: SimulationSourcePin,
    model: ProcessModel,
    acl: SimulationAclSnapshot,
    actor: ProcessActor,
    clock: SimulationClock,
    ids: Rc<RefCell<SimulationIdSource>>,
}

pub(crate) struct SimulationOpenError {
    pub(crate) database: SimulationDatabase,
    pub(crate) error: anyhow::Error,
}

impl SimulationStore {
    pub(crate) fn create(source: AuthenticatedSimulationSource) -> Result<Self> {
        Self::create_in_database(SimulationDatabase::open()?, source)
    }

    pub(crate) fn create_in_database(
        database: SimulationDatabase,
        source: AuthenticatedSimulationSource,
    ) -> Result<Self> {
        let input = source.input().clone();
        let (model, acl) = validate_captured_source(&input)?;
        repository::install_simulation_source(&database.conn, &source, &model)?;
        let simulation_id = Uuid::new_v4().to_string();
        let ids = SimulationIdSource::new(&input.scenario_sha256)?;
        let clock = SimulationClock::new(input.start_ms, input.horizon_ms, input.tick_duration_ms)?;
        let source = SimulationSourcePin {
            simulation_id: simulation_id.clone(),
            org_id: input.org_id.clone(),
            owner_user_id: input.owner_user_id.clone(),
            actor_user_id: input.actor_user_id.clone(),
            definition_id: input.definition_id.clone(),
            version: input.version,
            model_sha256: input.model_sha256.clone(),
            model_json: input.model_json.clone(),
            selected_process_id: input.selected_process_id.clone(),
            start_node_id: input.start_node_id.clone(),
            acl_snapshot_json: input.acl_snapshot_json.clone(),
        };
        let actor = ProcessActor {
            org_id: input.org_id,
            user_id: input.actor_user_id,
        };
        let ids = Rc::new(RefCell::new(ids));
        let store = Self {
            conn: database.into_connection(),
            simulation_id,
            source,
            model,
            acl,
            actor,
            clock,
            ids,
        };
        store.insert_identity(&input)?;
        ensure!(
            store.resident_bytes()? <= MAX_SIMULATION_DATABASE_BYTES,
            "simulation private database exceeds its resident size budget"
        );
        Ok(store)
    }

    pub(crate) fn open(
        database: SimulationDatabase,
        simulation_id: &str,
        actor: ProcessActor,
        authorization: AuthenticatedSimulationAction,
    ) -> std::result::Result<Self, SimulationOpenError> {
        macro_rules! retain_database {
            ($result:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(error) => return Err(SimulationOpenError { database, error }),
                }
            };
        }

        if Uuid::parse_str(simulation_id).is_err() {
            return Err(SimulationOpenError {
                database,
                error: anyhow::anyhow!("simulation identifier is malformed"),
            });
        }
        let source = retain_database!(load_source(&database.conn, simulation_id));
        retain_database!(authorization.verify(&source, &actor));
        let scenario_sha256 = retain_database!(load_scenario_sha(&database.conn, simulation_id));
        let input = SimulationSourceInput {
            org_id: source.org_id.clone(),
            owner_user_id: source.owner_user_id.clone(),
            actor_user_id: source.actor_user_id.clone(),
            definition_id: source.definition_id.clone(),
            version: source.version,
            model_json: source.model_json.clone(),
            model_sha256: source.model_sha256.clone(),
            selected_process_id: source.selected_process_id.clone(),
            start_node_id: source.start_node_id.clone(),
            acl_snapshot_json: source.acl_snapshot_json.clone(),
            scenario_sha256,
            start_ms: 0,
            horizon_ms: 1,
            tick_duration_ms: 1,
        };
        let (model, acl) = retain_database!(validate_captured_source(&input));
        retain_database!(ensure_actor_allowed(&source, &acl, &actor));
        let clock = retain_database!(load_clock(&database.conn, simulation_id));
        let mut ids = retain_database!(SimulationIdSource::new(&input.scenario_sha256));
        ids.set_step_index(clock.step_index);
        let conn = database.into_connection();
        Ok(Self {
            conn,
            simulation_id: simulation_id.to_owned(),
            source,
            model,
            acl,
            actor,
            clock,
            ids: Rc::new(RefCell::new(ids)),
        })
    }

    pub(crate) fn source_pin(
        database: &SimulationDatabase,
        simulation_id: &str,
    ) -> Result<SimulationSourcePin> {
        ensure!(
            Uuid::parse_str(simulation_id).is_ok(),
            "simulation identifier is malformed"
        );
        load_source(&database.conn, simulation_id)
    }

    pub(crate) fn simulation_id(&self) -> &str {
        &self.simulation_id
    }
    pub(crate) fn source(&self) -> &SimulationSourcePin {
        &self.source
    }
    pub(crate) fn clock(&self) -> &SimulationClock {
        &self.clock
    }
    pub(crate) fn into_database(self) -> SimulationDatabase {
        SimulationDatabase { conn: self.conn }
    }

    #[cfg(test)]
    pub(crate) fn private_row_snapshot(&self) -> Result<BTreeMap<String, Vec<Vec<String>>>> {
        let mut table_query = self.conn.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let table_names = table_query
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut snapshot = BTreeMap::new();
        for table_name in table_names {
            let quoted_name = format!("\"{}\"", table_name.replace('"', "\"\""));
            let mut rows_query = self
                .conn
                .prepare(&format!("SELECT * FROM {quoted_name}"))?;
            let column_count = rows_query.column_count();
            let mut rows = rows_query.query([])?;
            let mut table_rows = Vec::new();
            while let Some(row) = rows.next()? {
                let mut values = Vec::with_capacity(column_count);
                for column in 0..column_count {
                    let value: rusqlite::types::Value = row.get(column)?;
                    values.push(format!("{value:?}"));
                }
                table_rows.push(values);
            }
            table_rows.sort();
            snapshot.insert(table_name, table_rows);
        }
        Ok(snapshot)
    }

    fn resident_bytes(&self) -> Result<u64> {
        connection_resident_bytes(&self.conn)
    }

    pub(crate) fn start(
        &mut self,
        authorization: AuthenticatedSimulationAction,
        variables: Value,
    ) -> Result<SimulationView> {
        authorization.verify(&self.source, &self.actor)?;
        ensure!(
            load_instance_id(&self.conn, &self.simulation_id)?.is_none(),
            "simulation already has an instance"
        );
        validate_variables(&variables)?;
        let now = self.clock.now_ms;
        let instance_id = self.next_id("instance");
        let command_id = self.next_id("command");
        let request_hash = hash_json(&json!({"action":"start","variables":variables}))?;
        let handle = self.id_handle();
        let plan = plan_start_with_id_source(
            &self.model,
            &self.source.selected_process_id,
            &self.source.start_node_id,
            &instance_id,
            &self.actor,
            &self.source.definition_id,
            self.source.version,
            variables,
            StartCause::Manual,
            now,
            StartInputRef::Manual {
                command_id: command_id.clone(),
                request_hash: request_hash.clone(),
            },
            None,
            handle,
        )?;
        let accepted = AcceptedInputRef::Start {
            instance_id: instance_id.clone(),
            cause: StartInputRef::Manual {
                command_id: command_id.clone(),
                request_hash: request_hash.clone(),
            },
        };
        self.apply_plan(command_id, request_hash, 1, plan, now, Some(accepted), None, None)?;
        self.view_authorized()
    }

    pub(crate) fn advance(
        &mut self,
        authorization: AuthenticatedSimulationAction,
    ) -> Result<SimulationView> {
        authorization.verify(&self.source, &self.actor)?;
        let snapshot = self
            .load_snapshot()?
            .context("simulation has not been started")?;
        let expected_revision = snapshot.instance.revision;
        let now = self.clock.next_time()?;
        self.prepare_ids();
        let due_timer = snapshot
            .timers
            .iter()
            .filter_map(|timer| {
                let due_at_ms = timer.due_at_ms?;
                (timer.kind == tentaflow_protocol::processes::ProcessTimerKind::Catch
                    && matches!(
                        timer.status,
                        ProcessTimerStatus::Pending | ProcessTimerStatus::Blocked
                    )
                    && due_at_ms <= now
                    && timer.next_check_at_ms <= now)
                .then_some((due_at_ms, timer))
            })
            .min_by_key(|(due_at_ms, timer)| (*due_at_ms, timer.timer_id.clone()))
            .map(|(due_at_ms, timer)| DueTimer {
                timer_id: timer.timer_id.clone(),
                kind: timer.kind.clone(),
                org_id: timer.org_id.clone(),
                definition_id: timer.definition_id.clone(),
                version: timer.version,
                start_process_id: timer.start_process_id.clone(),
                instance_id: timer.instance_id.clone(),
                token_id: timer.token_id.clone(),
                occurrence: timer.occurrence,
                revision: timer.revision,
                due_at_ms,
            });
        let plan = if let Some(timer) = due_timer.as_ref() {
            let timer = snapshot
                .timers
                .iter()
                .find(|candidate| candidate.timer_id == timer.timer_id)
                .cloned()
                .context("selected simulation timer disappeared from its snapshot")?;
            let timer_snapshot = TimerSnapshot::Catch {
                actor: self.actor.clone(),
                timer,
                snapshot: snapshot.clone(),
            };
            super::timers::plan_timer_fire(
                &timer_snapshot,
                now,
                None,
                Some(self.id_handle()),
            )?
        } else {
            plan_advance_with_id_source(&snapshot, now, None, self.id_handle())?
        };
        let command_id = self.next_id("command");
        #[cfg(test)]
        run_simulation_transition_preflight(&self.conn, &command_id)?;
        let request_hash =
            hash_json(&json!({"action":"advance","revision":expected_revision,"at_ms":now}))?;
        self.apply_plan(
            command_id,
            request_hash,
            expected_revision,
            plan,
            now,
            None,
            None,
            due_timer.as_ref(),
        )?;
        self.view_authorized()
    }

    pub(crate) fn complete_user_task(
        &mut self,
        authorization: AuthenticatedSimulationAction,
        task_id: &str,
        outputs: Value,
    ) -> Result<SimulationView> {
        authorization.verify(&self.source, &self.actor)?;
        let snapshot = self
            .load_snapshot()?
            .context("simulation has not been started")?;
        let task = snapshot
            .user_tasks
            .iter()
            .find(|task| task.user_task_id == task_id)
            .context("simulation user task was not found")?;
        ensure!(
            task.can_complete,
            "simulation task is assigned to another actor"
        );
        ensure!(
            task.kind == ProcessUserTaskKind::Work,
            "task is not a Work user task"
        );
        ensure!(
            self.acl
                .permitted_user_ids
                .iter()
                .any(|id| id == &self.actor.user_id),
            "actor is outside the captured simulation ACL"
        );
        let now = self.clock.next_time()?;
        self.prepare_ids();
        let command_id = self.next_id("command");
        let request_hash =
            hash_json(&json!({"action":"complete_user_task","task_id":task_id,"outputs":outputs}))?;
        let accepted = AcceptedInputRef::Human {
            task_id: task_id.to_owned(),
            expected_task_revision: task.revision,
            expected_instance_revision: snapshot.instance.revision,
            command_id: command_id.clone(),
            request_hash: request_hash.clone(),
        };
        let plan = plan_user_completion_with_id_source(
            &snapshot,
            task_id,
            &outputs,
            None,
            now,
            accepted.clone(),
            None,
            self.id_handle(),
        )?;
        self.apply_plan(
            command_id,
            request_hash,
            snapshot.instance.revision,
            plan,
            now,
            Some(accepted),
            Some(outputs),
            None,
        )?;
        self.view_authorized()
    }

    pub(crate) fn acknowledge_manual_task(
        &mut self,
        authorization: AuthenticatedSimulationAction,
        task_id: &str,
    ) -> Result<SimulationView> {
        authorization.verify(&self.source, &self.actor)?;
        let snapshot = self
            .load_snapshot()?
            .context("simulation has not been started")?;
        let task = snapshot
            .user_tasks
            .iter()
            .find(|task| task.user_task_id == task_id)
            .context("simulation manual task was not found")?;
        ensure!(
            task.can_complete,
            "simulation task is assigned to another actor"
        );
        ensure!(
            task.kind == ProcessUserTaskKind::Manual,
            "task is not a Manual task"
        );
        ensure!(
            self.acl
                .permitted_user_ids
                .iter()
                .any(|id| id == &self.actor.user_id),
            "actor is outside the captured simulation ACL"
        );
        let now = self.clock.next_time()?;
        self.prepare_ids();
        let command_id = self.next_id("command");
        let request_hash =
            hash_json(&json!({"action":"acknowledge_manual_task","task_id":task_id}))?;
        let accepted = AcceptedInputRef::ManualAcknowledgment {
            task_id: task_id.to_owned(),
            expected_task_revision: task.revision,
            expected_instance_revision: snapshot.instance.revision,
            command_id: command_id.clone(),
            request_hash: request_hash.clone(),
        };
        let plan = plan_manual_acknowledgment_with_id_source(
            &snapshot,
            task_id,
            &self.actor.user_id,
            now,
            accepted.clone(),
            None,
            self.id_handle(),
        )?;
        self.apply_plan(
            command_id,
            request_hash,
            snapshot.instance.revision,
            plan,
            now,
            Some(accepted),
            None,
            None,
        )?;
        self.view_authorized()
    }

    pub(crate) fn view(
        &self,
        authorization: AuthenticatedSimulationAction,
    ) -> Result<SimulationView> {
        authorization.verify(&self.source, &self.actor)?;
        self.view_authorized()
    }

    fn view_authorized(&self) -> Result<SimulationView> {
        ensure_actor_allowed(&self.source, &self.acl, &self.actor)?;
        let snapshot = self.load_snapshot()?;
        let user_tasks = match snapshot.as_ref() {
            Some(snapshot) => repository::simulation_user_tasks_on(
                &self.conn,
                &self.actor,
                &snapshot.instance.instance_id,
            )?,
            None => Vec::new(),
        };
        let timers = match snapshot.as_ref() {
            Some(snapshot) => repository::simulation_timer_summaries_on(
                &self.conn,
                &self.actor,
                &snapshot.instance.instance_id,
            )?,
            None => Vec::new(),
        };
        Ok(SimulationView {
            simulation_id: self.simulation_id.clone(),
            source: self.source.clone(),
            clock: self.clock.clone(),
            instance: snapshot.as_ref().map(|snapshot| snapshot.instance.clone()),
            user_tasks,
            timers,
            tokens: snapshot
                .as_ref()
                .map(|snapshot| snapshot.tokens.clone())
                .unwrap_or_default(),
            gateway_receipts: snapshot
                .as_ref()
                .map(|snapshot| snapshot.receipts.clone())
                .unwrap_or_default(),
            incidents: snapshot
                .as_ref()
                .map(|snapshot| snapshot.incidents.clone())
                .unwrap_or_default(),
            events: self.load_events()?,
            trace_steps: self.load_trace_steps()?,
            activity_io_witnesses: snapshot
                .as_ref()
                .map(|snapshot| snapshot.activity_io_witnesses.clone())
                .unwrap_or_default(),
        })
    }

    fn insert_identity(&self, input: &SimulationSourceInput) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .context("begin simulation identity transaction")?;
        tx.execute(
            "INSERT INTO simulation_meta(simulation_id,scenario_sha256,profile,org_id,actor_user_id,definition_id,version,model_sha256,status,revision,instance_id,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'running',1,NULL,?9,?9)",
            params![self.simulation_id, input.scenario_sha256, PROFILE_NAME, input.org_id, input.actor_user_id, input.definition_id, input.version, input.model_sha256, input.start_ms],
        )?;
        tx.execute(
            "INSERT INTO simulation_source_pins(simulation_id,org_id,owner_user_id,actor_user_id,definition_id,version,model_sha256,model_json,selected_process_id,start_node_id,acl_snapshot_json,captured_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![self.simulation_id, input.org_id, input.owner_user_id, input.actor_user_id, input.definition_id, input.version, input.model_sha256, input.model_json, input.selected_process_id, input.start_node_id, input.acl_snapshot_json, input.start_ms],
        )?;
        tx.execute(
            "INSERT INTO simulation_clock(simulation_id,start_ms,now_ms,horizon_ms,tick_duration_ms,step_index,revision) VALUES(?1,?2,?2,?3,?4,0,1)",
            params![self.simulation_id, input.start_ms, input.horizon_ms, input.tick_duration_ms],
        )?;
        tx.commit().context("commit simulation identity")?;
        Ok(())
    }

    fn id_handle(&self) -> RuntimeIdSourceHandle {
        self.ids.clone()
    }

    fn prepare_ids(&mut self) {
        self.ids.borrow_mut().set_step_index(self.clock.step_index);
    }

    fn next_id(&mut self, kind: &str) -> String {
        self.ids.borrow_mut().next_id(kind)
    }

    fn apply_plan(
        &mut self,
        command_id: String,
        request_hash: String,
        expected_revision: u64,
        plan: RuntimePlan,
        at_ms: i64,
        accepted: Option<AcceptedInputRef>,
        human_outputs: Option<Value>,
        timer_candidate: Option<&DueTimer>,
    ) -> Result<()> {
        ensure!(
            self.clock.now_ms <= at_ms && at_ms <= self.clock.horizon_ms,
            "simulation transition timestamp is outside the virtual clock"
        );
        ensure!(
            expected_revision == self.clock.revision,
            "simulation revision conflict"
        );
        ensure!(
            self.ids.borrow().accepts(&command_id),
            "simulation command id was not issued by its allocator"
        );
        ensure!(
            self.ids.borrow().error().is_none(),
            "simulation identifier allocation failed"
        );
        let mut plan = plan;
        for event_index in 0..plan.events.len() {
            if !plan.event_ids.contains_key(&event_index) {
                let event_id = self.next_id("event");
                plan.event_ids.insert(event_index, event_id);
            }
        }
        ensure!(
            self.ids.borrow().error().is_none(),
            "simulation event identifier allocation failed"
        );
        validate_plan_ids(&plan, &self.ids.borrow())?;
        validate_supported_plan(&plan)?;

        let tx = self
            .conn
            .unchecked_transaction()
            .context("begin simulation transition")?;
        let result = if let Some(instance_id) = plan.start_instance_id.as_deref() {
            ensure!(
                load_instance_id_tx(&tx, &self.simulation_id)?.is_none(),
                "simulation already has an instance"
            );
            let accepted = accepted
                .as_ref()
                .context("simulation start has no accepted command")?;
            ensure!(
                matches!(accepted, AcceptedInputRef::Start { instance_id: started, .. }
                    if started == instance_id),
                "simulation start command identity differs from its accepted input"
            );
            repository::start_simulation_plan_on(
                &tx,
                &self.actor,
                instance_id,
                &self.source.definition_id,
                self.source.version,
                &self.source.selected_process_id,
                &self.source.start_node_id,
                &plan,
                at_ms,
                accepted,
            )?
        } else if let Some(candidate) = timer_candidate {
            let instance_id = load_instance_id_tx(&tx, &self.simulation_id)?
                .context("simulation timer plan has no instance")?;
            ensure!(
                candidate.instance_id.as_deref() == Some(instance_id.as_str()),
                "simulation timer candidate belongs to another instance"
            );
            let timer_outcome = repository::fire_timer_on(
                &tx,
                candidate,
                &self.actor,
                Some(expected_revision),
                &plan,
                at_ms,
                false,
            )?
            .context("simulation timer changed before its private commit")?;
            ensure!(
                timer_outcome.cancelled_claims.is_empty(),
                "simulation TimerCatch produced unsupported cancelled worker claims"
            );
            timer_outcome.instance
        } else {
            let instance_id = load_instance_id_tx(&tx, &self.simulation_id)?
                .context("simulation plan has no instance")?;
            repository::apply_simulation_plan_on(
                &tx,
                &self.actor,
                &instance_id,
                expected_revision,
                &plan,
                at_ms,
                accepted.as_ref(),
                human_outputs.as_ref(),
            )?
        };

        let mut next_clock = self.clock.clone();
        next_clock.accept_transition(at_ms)?;
        tx.execute(
            "UPDATE simulation_clock SET now_ms=?1,step_index=?2,revision=?3 WHERE simulation_id=?4 AND revision=?5",
            params![
                next_clock.now_ms,
                next_clock.step_index as i64,
                next_clock.revision as i64,
                self.simulation_id,
                self.clock.revision as i64
            ],
        )?;
        ensure!(tx.changes() == 1, "simulation clock changed before commit");

        let event_ids = plan
            .event_ids
            .iter()
            .map(|(index, event_id)| (*index, event_id.clone()))
            .collect::<BTreeMap<_, _>>();
        let result_json = json!({
            "status": simulation_meta_status(&result.status),
            "revision": result.revision,
            "events": event_ids,
        });
        let result_json_text = json_string(&result_json)?;
        tx.execute(
            "UPDATE simulation_meta SET instance_id=?1,status=?2,revision=?3,updated_at_ms=?4 WHERE simulation_id=?5 AND revision=?6",
            params![
                result.instance_id,
                simulation_meta_status(&result.status),
                result.revision as i64,
                at_ms,
                self.simulation_id,
                self.clock.revision as i64
            ],
        )?;
        ensure!(
            tx.changes() == 1,
            "simulation identity changed before commit"
        );
        tx.execute(
            "INSERT INTO simulation_commands(command_id,simulation_id,actor_user_id,request_hash,expected_revision,result_json,at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                command_id,
                self.simulation_id,
                self.actor.user_id,
                request_hash,
                expected_revision as i64,
                result_json_text,
                at_ms
            ],
        )?;
        let trace_id = self.next_id("trace_step");
        ensure!(
            self.ids.borrow().error().is_none(),
            "simulation trace identifier allocation failed"
        );
        let ordinal: i64 = tx.query_row(
            "SELECT COALESCE(MAX(ordinal),-1)+1 FROM simulation_trace_steps WHERE simulation_id=?1",
            [&self.simulation_id],
            |row| row.get(0),
        )?;
        let result_hash = hash_json(&result_json)?;
        let trace_data = json!({
            "command_id": command_id,
            "result": result_json,
        });
        let trace_data_text = json_string(&trace_data)?;
        ensure!(
            trace_data_text.len() <= MAX_TRACE_BYTES,
            "simulation trace step exceeds its bound"
        );
        tx.execute(
            "INSERT INTO simulation_trace_steps(trace_step_id,simulation_id,ordinal,action,at_ms,request_sha256,result_sha256,data_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                trace_id,
                self.simulation_id,
                ordinal,
                "transition",
                at_ms,
                request_hash,
                result_hash,
                trace_data_text
            ],
        )?;
        let page_count: i64 = tx.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: i64 = tx.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        let resident_bytes = u64::try_from(page_count)
            .context("private simulation page count is negative")?
            .checked_mul(
                u64::try_from(page_size).context("private simulation page size is negative")?,
            )
            .context("private simulation resident size overflow")?;
        ensure!(
            resident_bytes <= MAX_SIMULATION_DATABASE_BYTES,
            "simulation transition exceeds its resident size budget"
        );
        tx.commit().context("commit simulation transition")?;
        self.clock = next_clock;
        Ok(())
    }

    fn load_snapshot(&self) -> Result<Option<RuntimeSnapshot>> {
        let Some(instance_id) = load_instance_id(&self.conn, &self.simulation_id)? else {
            return Ok(None);
        };
        repository::runtime_snapshot_on(&self.conn, &self.actor, &instance_id).map(Some)
    }

    fn load_events(&self) -> Result<Vec<SimulationEventView>> {
        let Some(instance_id) = load_instance_id(&self.conn, &self.simulation_id)? else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            "SELECT event_id,seq,at_ms,kind,node_id,actor_user_id,data_json,scope_id
             FROM bpmn_events WHERE instance_id=?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([instance_id], |row| {
            Ok(SimulationEventView {
                event_id: row.get(0)?,
                seq: row.get::<_, i64>(1)? as u64,
                at_ms: row.get(2)?,
                kind: row.get(3)?,
                node_id: row.get(4)?,
                actor_user_id: row.get(5)?,
                data: serde_json::from_str(&row.get::<_, String>(6)?)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
                scope_id: row.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn load_trace_steps(&self) -> Result<Vec<SimulationTraceStep>> {
        let mut stmt = self.conn.prepare("SELECT trace_step_id,ordinal,action,at_ms,request_sha256,result_sha256,data_json FROM simulation_trace_steps WHERE simulation_id=?1 ORDER BY ordinal")?;
        let rows = stmt.query_map([&self.simulation_id], |row| {
            Ok(SimulationTraceStep {
                trace_step_id: row.get(0)?,
                ordinal: row.get::<_, i64>(1)? as u64,
                action: row.get(2)?,
                at_ms: row.get(3)?,
                request_sha256: row.get(4)?,
                result_sha256: row.get(5)?,
                data: serde_json::from_str(&row.get::<_, String>(6)?)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

fn ensure_actor_allowed(
    source: &SimulationSourcePin,
    acl: &SimulationAclSnapshot,
    actor: &ProcessActor,
) -> Result<()> {
    ensure!(
        actor.org_id == source.org_id,
        "simulation actor belongs to another organization"
    );
    ensure!(
        acl.permitted_user_ids.iter().any(|id| id == &actor.user_id),
        "simulation actor is outside the captured ACL"
    );
    Ok(())
}

fn validate_captured_source(
    input: &SimulationSourceInput,
) -> Result<(ProcessModel, SimulationAclSnapshot)> {
    ensure!(
        !input.org_id.is_empty()
            && !input.owner_user_id.is_empty()
            && !input.actor_user_id.is_empty(),
        "simulation source identity is incomplete"
    );
    ensure!(
        !input.definition_id.is_empty() && input.version > 0,
        "simulation definition identity is invalid"
    );
    validate_sha256(&input.model_sha256, "model SHA-256")?;
    validate_sha256(&input.scenario_sha256, "scenario SHA-256")?;
    ensure!(
        input.model_json.len() <= 8 * 1024 * 1024,
        "simulation model exceeds its capture bound"
    );
    ensure!(
        input.acl_snapshot_json.len() <= MAX_ACL_BYTES,
        "simulation ACL snapshot exceeds its bound"
    );
    let computed = hex::encode(Sha256::digest(input.model_json.as_bytes()));
    ensure!(
        computed == input.model_sha256,
        "simulation model hash does not match captured bytes"
    );
    let model: ProcessModel =
        serde_json::from_str(&input.model_json).context("decode captured process model")?;
    validate_model(&model).context("validate captured process model")?;
    let acl: SimulationAclSnapshot =
        serde_json::from_str(&input.acl_snapshot_json).context("decode captured simulation ACL")?;
    ensure!(
        !acl.permitted_user_ids.is_empty(),
        "simulation ACL snapshot is empty"
    );
    ensure!(
        acl.permitted_user_ids.iter().all(|id| !id.is_empty()),
        "simulation ACL contains an empty user id"
    );
    ensure!(
        acl.permitted_user_ids
            .iter()
            .any(|id| id == &input.actor_user_id),
        "simulation actor is outside captured ACL"
    );
    ensure!(
        acl.permitted_user_ids
            .iter()
            .any(|id| id == &input.owner_user_id),
        "simulation owner is outside captured ACL"
    );
    ensure!(
        model.additional_processes.is_empty(),
        SimulationUnsupportedActivity::AdditionalProcess
    );
    let body = selected_body(&model, &input.selected_process_id, &[])?;
    let start = body
        .nodes
        .iter()
        .find(|node| node.id == input.start_node_id)
        .context("captured simulation Start is absent")?;
    ensure!(
        matches!(&start.kind, ProcessNodeKind::Start),
        "simulation initial profile requires a plain Start event"
    );
    ensure!(
        body.nodes
            .iter()
            .filter(|node| matches!(&node.kind, ProcessNodeKind::Start))
            .count()
            == 1,
        "simulation initial profile requires one Start event"
    );
    ensure!(
        body.nodes
            .iter()
            .any(|node| matches!(&node.kind, ProcessNodeKind::End)),
        "simulation initial profile requires an End event"
    );
    for node in body.nodes {
        if node.repeat.is_some() {
            return Err(SimulationUnsupportedActivity::Repetition {
                node_id: node.id.clone(),
            }
            .into());
        }
        match &node.kind {
            ProcessNodeKind::Start
            | ProcessNodeKind::End
            | ProcessNodeKind::ScriptTask { .. }
            | ProcessNodeKind::UserTask { .. }
            | ProcessNodeKind::ManualTask { .. }
            | ProcessNodeKind::ExclusiveGateway { .. }
            | ProcessNodeKind::ParallelGateway
            | ProcessNodeKind::InclusiveGateway { .. } => {}
            ProcessNodeKind::TimerCatch { timer } => {
                ensure!(
                    matches!(
                        timer,
                        ProcessTimerSpec::Date { .. }
                            | ProcessTimerSpec::Duration { .. }
                            | ProcessTimerSpec::WorkingDuration { .. }
                    ),
                    "simulation supports Date, Duration, and WorkingDuration TimerCatch rules"
                );
                ensure!(
                    model.timer_timezone.as_deref().is_some_and(|zone| !zone.is_empty()),
                    "TimerCatch simulation requires the published IANA timer timezone"
                );
                if matches!(timer, ProcessTimerSpec::WorkingDuration { .. }) {
                    ensure!(
                        model.calendar_pin.is_some(),
                        "WorkingDuration TimerCatch simulation requires the published calendar pin"
                    );
                }
            }
            ProcessNodeKind::ServiceTask { .. } => {
                return Err(SimulationUnsupportedActivity::ServiceTask {
                    node_id: node.id.clone(),
                }
                .into())
            }
            ProcessNodeKind::CallActivity(_) => {
                return Err(SimulationUnsupportedActivity::CallActivity {
                    node_id: node.id.clone(),
                }
                .into())
            }
            ProcessNodeKind::SubProcess { .. } => {
                return Err(SimulationUnsupportedActivity::SubProcess {
                    node_id: node.id.clone(),
                }
                .into())
            }
            kind => {
                return Err(SimulationUnsupportedActivity::ExternalActivity {
                    node_id: node.id.clone(),
                    kind: format!("{kind:?}"),
                }
                .into())
            }
        }
    }
    Ok((model, acl))
}

fn validate_plan_ids(plan: &RuntimePlan, ids: &SimulationIdSource) -> Result<()> {
    ensure!(
        plan.event_ids.len() == plan.events.len()
            && (0..plan.events.len()).all(|index| plan.event_ids.contains_key(&index)),
        "simulation plan must provide a deterministic UUID for every event"
    );
    for index in plan.event_ids.keys() {
        ensure!(
            *index < plan.events.len(),
            "simulation plan contains an event identifier outside its event ledger"
        );
    }
    for id in plan
        .create_tokens
        .iter()
        .map(|token| token.token_id.as_str())
        .chain(
            plan.create_user_tasks
                .iter()
                .map(|task| task.user_task_id.as_str()),
        )
        .chain(
            plan.add_incidents
                .iter()
                .map(|incident| incident.incident_id.as_str()),
        )
        .chain(plan.event_ids.values().map(String::as_str))
        .chain(
            plan.add_gateway_receipts
                .iter()
                .flat_map(|receipt| [receipt.activation_id.as_str(), receipt.token_id.as_str()]),
        )
        .chain(plan.create_timers.iter().map(|timer| timer.timer_id.as_str()))
        .chain(plan.activity_io_inputs.iter().map(|fact| match fact {
            ActivityIoInputFact::Captured { witness_id, .. }
            | ActivityIoInputFact::Failed { witness_id, .. } => witness_id.as_str(),
        }))
    {
        ensure!(
            Uuid::parse_str(id).is_ok(),
            "simulation plan contains a malformed UUID"
        );
        ensure!(
            ids.accepts(id),
            "simulation plan contains an identifier not issued by its allocator"
        );
    }
    for id in plan
        .remove_gateway_receipts
        .iter()
        .flat_map(|receipt| [receipt.activation_id.as_str(), receipt.token_id.as_str()])
    {
        ensure!(
            Uuid::parse_str(id).is_ok(),
            "simulation plan contains a malformed existing gateway UUID"
        );
    }
    Ok(())
}

fn validate_supported_plan(plan: &RuntimePlan) -> Result<()> {
    ensure!(
        plan.create_jobs.is_empty()
            && plan.create_messages.is_empty()
            && plan.create_signals.is_empty(),
        "simulation plan contains an external dispatch"
    );
    ensure!(
        plan.service_invocation_ids.is_empty()
            && plan.closed_service_dispatches.is_empty()
            && plan.complete_job_ids.is_empty()
            && plan.cancel_job_ids.is_empty(),
        "simulation plan contains a Service dispatch"
    );
    ensure!(
        plan.activity_io_continuations.is_empty() && plan.recovered_service_incidents.is_empty(),
        "simulation plan contains a deferred external continuation"
    );
    ensure!(
        plan.create_timers
            .iter()
            .all(|timer| timer.kind == tentaflow_protocol::processes::ProcessTimerKind::Catch)
            && plan.subscription_updates.is_empty()
            && plan.create_subscriptions.is_empty()
            && plan.race_updates.is_empty()
            && plan.create_event_races.is_empty(),
        "the simulation profile only supports Catch timers and no event subscriptions"
    );
    ensure!(
        plan.call_requests.is_empty()
            && plan.call_steps.is_empty()
            && plan.create_scopes.is_empty()
            && plan.scope_updates.is_empty()
            && plan.cancel_scope_roots.is_empty()
            && plan.scope_terminal_errors.is_empty(),
        "nested process execution is unsupported in the initial simulation profile"
    );
    ensure!(
        plan.repetition_groups.is_empty()
            && plan.repetition_occurrences.is_empty()
            && plan.repetition_capacity.is_none(),
        "repetition is unsupported in the initial simulation profile"
    );
    ensure!(
        plan.business_error.is_none() && plan.terminal_error.is_none(),
        "business and terminal errors are unsupported in the initial simulation profile"
    );
    ensure!(
        plan.create_user_tasks.iter().all(|task| matches!(
            task.kind,
            ProcessUserTaskKind::Work | ProcessUserTaskKind::Manual
        )),
        "verification tasks are unsupported in the initial simulation profile"
    );
    ensure!(
        plan.termination_attempts.iter().all(|attempt| {
            let accepted = match attempt {
                super::repository::TerminationAttempt::Success(source) => &source.accepted_input,
                super::repository::TerminationAttempt::ReturnFailure(failure) => {
                    &failure.accepted_input
                }
            };
            matches!(
                accepted,
                AcceptedInputRef::Start { .. }
                    | AcceptedInputRef::PersistedReady { .. }
                    | AcceptedInputRef::Human { .. }
                    | AcceptedInputRef::ManualAcknowledgment { .. }
                    | AcceptedInputRef::Timer { .. }
            )
        }),
        "termination source is outside the initial simulation profile"
    );
    Ok(())
}

fn load_source(conn: &Connection, simulation_id: &str) -> Result<SimulationSourcePin> {
    conn.query_row("SELECT simulation_id,org_id,owner_user_id,actor_user_id,definition_id,version,model_sha256,model_json,selected_process_id,start_node_id,acl_snapshot_json FROM simulation_source_pins WHERE simulation_id=?1", [simulation_id], |row| Ok(SimulationSourcePin { simulation_id: row.get(0)?, org_id: row.get(1)?, owner_user_id: row.get(2)?, actor_user_id: row.get(3)?, definition_id: row.get(4)?, version: row.get(5)?, model_sha256: row.get(6)?, model_json: row.get(7)?, selected_process_id: row.get(8)?, start_node_id: row.get(9)?, acl_snapshot_json: row.get(10)? })).map_err(Into::into)
}

fn load_scenario_sha(conn: &Connection, simulation_id: &str) -> Result<String> {
    conn.query_row(
        "SELECT scenario_sha256 FROM simulation_meta WHERE simulation_id=?1",
        [simulation_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn load_clock(conn: &Connection, simulation_id: &str) -> Result<SimulationClock> {
    conn.query_row("SELECT start_ms,now_ms,horizon_ms,tick_duration_ms,step_index,revision FROM simulation_clock WHERE simulation_id=?1", [simulation_id], |row| Ok(SimulationClock { start_ms: row.get(0)?, now_ms: row.get(1)?, horizon_ms: row.get(2)?, tick_duration_ms: row.get(3)?, step_index: row.get::<_,i64>(4)? as u64, revision: row.get::<_,i64>(5)? as u64 })).map_err(Into::into)
}

fn load_instance_id(conn: &Connection, simulation_id: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT instance_id FROM simulation_meta WHERE simulation_id=?1",
        [simulation_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

fn load_instance_id_tx(tx: &Transaction<'_>, simulation_id: &str) -> Result<Option<String>> {
    tx.query_row(
        "SELECT instance_id FROM simulation_meta WHERE simulation_id=?1",
        [simulation_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            && value == value.to_ascii_lowercase(),
        "{label} must be lowercase hexadecimal SHA-256"
    );
    Ok(())
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn hash_json(value: &Value) -> Result<String> {
    Ok(hash_bytes(&serde_json::to_vec(value)?))
}
fn json_string<T: Serialize>(value: &T) -> Result<String> {
    json_string_with_limit(value, MAX_JSON_BYTES, "simulation JSON")
}
fn json_string_with_limit<T: Serialize>(value: &T, limit: usize, label: &str) -> Result<String> {
    let text = serde_json::to_string(value)?;
    ensure!(text.len() <= limit, "{label} exceeds its bound");
    Ok(text)
}
pub(super) fn simulation_meta_status(status: &ProcessInstanceStatus) -> &'static str {
    if matches!(status, ProcessInstanceStatus::Completed) {
        "completed"
    } else if matches!(
        status,
        ProcessInstanceStatus::Error | ProcessInstanceStatus::Cancelled
    ) {
        "failed"
    } else {
        "running"
    }
}

#[cfg(test)]
mod reservation_tests {
    use super::*;

    #[test]
    fn reservation_token_collision_preserves_the_existing_owner_slot() {
        let registry = SimulationRegistry::new();
        let first = registry
            .reserve_start_with_token("fixed-token".to_owned(), "owner-one")
            .expect("reserve the fixed token");
        let second = registry
            .reserve_start_with_token("owner-one-2".to_owned(), "owner-one")
            .expect("reserve the second owner slot");
        let third = registry
            .reserve_start_with_token("owner-one-3".to_owned(), "owner-one")
            .expect("reserve the third owner slot");
        let fourth = registry
            .reserve_start_with_token("owner-one-4".to_owned(), "owner-one")
            .expect("reserve the fourth owner slot");

        let collision = registry
            .reserve_start_with_token("fixed-token".to_owned(), "owner-two")
            .expect_err("a token collision must reject before replacing the reservation");
        assert!(format!("{collision:#}").contains("reservation collided"));

        let owner_limit = registry
            .reserve_start("owner-one")
            .expect_err("the original owner must still occupy all four slots");
        assert!(format!("{owner_limit:#}").contains("owner has reached"));

        drop((first, second, third, fourth));
    }
}
