// ===== File: sync/tentavm_registry.rs — the TentaVM registry on the ledger ====
//
// The eighteen `vm_*` tables of plan §4.1 replicate through Core Sync, and this
// module is BOTH directions of that: the capture a local write emits, and the
// arm that materializes an operation a peer sent. One module, because the two
// have to agree about the shape of a row, and the cheapest way to keep two
// things in agreement is to stop having two of them.
//
// Everything here is driven by the database schema, not by a hand-written column
// list per table:
//
//   * `capture_row` reads the row with `SELECT *` and states every column;
//   * `apply` writes the columns the operation states, after checking each one
//     against `PRAGMA table_info`;
//   * `reseed_table` calls the same `capture_row` the live write path calls.
//
// The reason is the failure this project has already paid for three times: one
// concept written down in several places in different words. A column added to
// `vm_hosts` by a later migration (the capacity columns of step 7 are exactly
// that) travels, materializes and re-seeds without anyone remembering to add it
// to a list — and `registry_capture_states_every_column` fails if that ever
// stops being true.
//
// AUTHORIZATION is plan §6.1 and is the part that is NOT generic: who may write
// a row depends on who owns it, and ownership is a property of the table.
// `OwnerRule` says how the owner of a row is found, and `authorize` applies the
// same four rules to every table:
//
//   * an Insert on a NEW key must come from the node it names as owner;
//   * an Insert on an EXISTING key is an Update — the owner of the row that is
//     already there wins, so an upsert can never take a row over;
//   * an Update or a Delete must come from the owner, or carry the next epoch;
//   * a satellite (a disk, a NIC, a snapshot, a membership) has no owner column
//     and resolves one through its machine and its host. No parent, no write.
//
// LWW stays what §6.1 says it is: an ORDER over the owner's own writes, never an
// authorization. A peer that does not own a row cannot even reach the version
// slot — `apply_core_operation` stamps it only after this arm returns Ok, and
// the transaction is rolled back when it does not.

use rusqlite::OptionalExtension;

use super::core_registry::{descriptor_for_kind, CoreSyncDescriptor, CoreSyncResourceKind as Kind};
use super::core_materializer::{
    field_string, operation_changes_nothing, optional_present_i64, optional_present_string,
    sql_error, table_columns, ColumnInfo,
};
use super::ledger::{ActionType, FieldValue, LedgerResult, SyncLedgerError, SyncOperation};

/// How the owner of a row in one registry table is found (plan §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerRule {
    /// The row names its owner itself, in `owner_node_id`. `vm_hosts` and
    /// `vm_connectors` also carry `owner_epoch`, which is what makes a transfer
    /// distinguishable from a takeover; `vm_jobs` does not, because a job is
    /// never handed over — a migration job that outlives its node is finished by
    /// nobody and re-created instead.
    Own,
    /// The row has no owner of its own: it belongs to the row named by
    /// `local_key` in `parent_table`, and so does its owner. The chain is walked
    /// until it reaches a table with `OwnerRule::Own`, which is why a disk
    /// resolves through its machine to the machine's host.
    ///
    /// `nullable` marks the two tables where the parent is optional
    /// (`vm_storage_pools`, `vm_networks`): `host_id IS NULL` is not a missing
    /// parent, it is the plan's "shared by several hosts", and such a row is
    /// governed by the organization instead.
    Parent {
        local_key: &'static str,
        parent_table: &'static str,
        nullable: bool,
    },
    /// Nobody's row: environment policy an administrator decides (`vm_tags`,
    /// `vm_instance_settings`, `vm_host_grants`) or the org-wide image catalog.
    ///
    /// Phase 0 has no way to check "the author is an administrator of this
    /// environment" — that needs the user signature of step 15, and
    /// `user_identity_keys` holds no key material yet. What the mesh DOES have
    /// is the operator list, which is exactly the answer plan §6.1 gives for the
    /// same question about `node_user_assignments`: the nodes that act for the
    /// organization. So these rows are writable from an operator node and from
    /// nowhere else, until step 15 can say more.
    Organization,
}

/// One registry table: how its rows are owned, and the columns whose values the
/// DDL constrains to a fixed set.
///
/// The enum lists are checked against the CHECK constraints of the migration by
/// `declared_enums_match_the_schema`, so this is not a second source of truth —
/// it is a readable copy the test keeps honest. Their point is the message: a
/// value the DDL forbids is refused here with a sentence naming the column,
/// instead of reaching SQLite and turning into a terminal conflict carrying raw
/// constraint text.
pub struct RegistryTable {
    pub kind: Kind,
    pub owner: OwnerRule,
    pub enum_columns: &'static [(&'static str, &'static [&'static str])],
}

pub const TENTAVM_REGISTRY_TABLES: &[RegistryTable] = &[
    RegistryTable {
        kind: Kind::VmHost,
        owner: OwnerRule::Own,
        enum_columns: &[("kind", &["node", "connector_host"])],
    },
    RegistryTable {
        kind: Kind::VmConnector,
        owner: OwnerRule::Own,
        enum_columns: &[],
    },
    RegistryTable {
        kind: Kind::VmConnectorSecretGrant,
        owner: OwnerRule::Parent {
            local_key: "connector_id",
            parent_table: "vm_connectors",
            nullable: false,
        },
        enum_columns: &[],
    },
    RegistryTable {
        kind: Kind::VmHostGpu,
        owner: OwnerRule::Parent {
            local_key: "host_id",
            parent_table: "vm_hosts",
            nullable: false,
        },
        enum_columns: &[],
    },
    RegistryTable {
        kind: Kind::VmStoragePool,
        owner: OwnerRule::Parent {
            local_key: "host_id",
            parent_table: "vm_hosts",
            nullable: true,
        },
        enum_columns: &[],
    },
    RegistryTable {
        kind: Kind::VmNetwork,
        owner: OwnerRule::Parent {
            local_key: "host_id",
            parent_table: "vm_hosts",
            nullable: true,
        },
        enum_columns: &[],
    },
    RegistryTable {
        kind: Kind::VmImage,
        owner: OwnerRule::Organization,
        enum_columns: &[],
    },
    RegistryTable {
        kind: Kind::VmImageLocation,
        owner: OwnerRule::Parent {
            local_key: "host_id",
            parent_table: "vm_hosts",
            nullable: false,
        },
        enum_columns: &[],
    },
    RegistryTable {
        kind: Kind::VmHostGrant,
        owner: OwnerRule::Organization,
        enum_columns: &[
            ("subject_kind", &["user", "group"]),
            ("role", &["view", "deploy", "manage"]),
            // Step 6: a grant an administrator typed into the H06 matrix versus
            // one COMPUTED from an approved access request. The distinction is
            // load-bearing, not descriptive — the matrix refuses to edit a
            // computed row, and a rejection arriving later removes one.
            ("source", &["grant_editor", "access_request"]),
        ],
    },
    RegistryTable {
        kind: Kind::VmInstanceSetting,
        owner: OwnerRule::Organization,
        enum_columns: &[],
    },
    RegistryTable {
        kind: Kind::VmGuest,
        owner: OwnerRule::Parent {
            local_key: "host_id",
            parent_table: "vm_hosts",
            nullable: false,
        },
        enum_columns: &[("kind", &["vm", "container", "system_container"])],
    },
    RegistryTable {
        kind: Kind::VmGuestMember,
        owner: OwnerRule::Parent {
            local_key: "guest_id",
            parent_table: "vm_guests",
            nullable: false,
        },
        enum_columns: &[
            ("subject_kind", &["user", "group"]),
            ("role", &["owner", "operator", "viewer"]),
        ],
    },
    RegistryTable {
        kind: Kind::VmGuestDisk,
        owner: OwnerRule::Parent {
            local_key: "guest_id",
            parent_table: "vm_guests",
            nullable: false,
        },
        enum_columns: &[],
    },
    RegistryTable {
        kind: Kind::VmGuestNic,
        owner: OwnerRule::Parent {
            local_key: "guest_id",
            parent_table: "vm_guests",
            nullable: false,
        },
        enum_columns: &[],
    },
    RegistryTable {
        kind: Kind::VmGuestDevice,
        owner: OwnerRule::Parent {
            local_key: "guest_id",
            parent_table: "vm_guests",
            nullable: false,
        },
        enum_columns: &[],
    },
    RegistryTable {
        kind: Kind::VmSnapshot,
        owner: OwnerRule::Parent {
            local_key: "guest_id",
            parent_table: "vm_guests",
            nullable: false,
        },
        enum_columns: &[(
            "kind",
            &["external_disk", "internal", "storage_clone"],
        )],
    },
    RegistryTable {
        kind: Kind::VmJob,
        owner: OwnerRule::Own,
        enum_columns: &[],
    },
    RegistryTable {
        kind: Kind::VmTag,
        owner: OwnerRule::Organization,
        enum_columns: &[],
    },
    // Both access-request tables are `Own`: the requester's node owns the
    // request, the deciding node owns its decision. That is the only choice the
    // §6.1 first-insert rule can actually CHECK — the author must be the node it
    // names as owner — and "the node that runs the environment" is not a thing:
    // an environment is installed on every node of the fleet.
    //
    // Both rows are immutable after their first insert (`refuse_rewrite`), so
    // the Update and Delete arms are unreachable for everyone including the
    // owner. That is what makes the audit unrewritable rather than
    // rewritable-by-one-node.
    RegistryTable {
        kind: Kind::VmAccessRequest,
        owner: OwnerRule::Own,
        enum_columns: &[
            ("scope", &["instance_create", "host_role"]),
            ("role", &["view", "deploy", "manage"]),
        ],
    },
    RegistryTable {
        kind: Kind::VmAccessDecision,
        owner: OwnerRule::Own,
        enum_columns: &[("decision", &["approve", "reject"])],
    },
];

/// The registry table for a resource kind, or `None` when the kind belongs to
/// some other part of core sync.
pub fn table_for_kind(kind: Kind) -> Option<&'static RegistryTable> {
    TENTAVM_REGISTRY_TABLES
        .iter()
        .find(|table| table.kind == kind)
}

fn registry_table(kind: Kind) -> LedgerResult<&'static RegistryTable> {
    table_for_kind(kind).ok_or_else(|| {
        SyncLedgerError::Runtime(format!("not a TentaVM registry resource: {kind:?}"))
    })
}

/// The columns that make up a row's replicated identity, in the order the
/// descriptor declares them.
pub fn key_columns(descriptor: &CoreSyncDescriptor) -> Vec<&'static str> {
    descriptor.primary_key_column.split(',').collect()
}

/// The ledger identity of a registry row. A single-column key travels as its own
/// value (that is what every other core descriptor does); a composite key
/// travels through the injective length-prefixed codec, so two different key
/// tuples can never collide into one resource.
pub fn registry_resource_id(key_values: &[String]) -> String {
    if key_values.len() == 1 {
        key_values[0].clone()
    } else {
        let parts: Vec<&str> = key_values.iter().map(String::as_str).collect();
        super::resource_id::composite_resource_id(&parts)
    }
}

/// The `owner_node_id` / `owner_epoch` a row currently holds. `epoch` is `None`
/// for a table that has no such column, which is not the same as epoch zero: it
/// means "this row is never transferred".
#[derive(Debug, Clone)]
struct Ownership {
    node: String,
    epoch: Option<i64>,
}

/// What `authorize` decided about an operation.
enum Verdict {
    /// The author may write; go ahead.
    Write,
    /// Nothing to do, and nothing to record. Used for a stale epoch (plan §6.1:
    /// "stara epoka = ignorowana") and for a Delete of a row this node does not
    /// have.
    Ignore,
}

// =============================================================================
// Apply — an operation from the wire
// =============================================================================

/// What one applied operation did, and — separately — whether its author was
/// allowed to do it.
///
/// The two are not the same question and conflating them costs the registry its
/// ordering: an operation that changes no rows may still be authorized (a
/// restatement of what the row already holds, which every reseed produces by
/// the hundred), and an operation that changes no rows may equally be one this
/// node refused. Only the first may move the row's position in the LWW order.
/// Reporting a bare row count let a peer with no title pin the order of
/// somebody else's row knowing only its resource id — and for a host of kind
/// `node` that id IS the public node id.
pub struct Applied {
    rows: usize,
    authorized: bool,
}

impl Applied {
    /// How many rows moved.
    pub(crate) fn rows(&self) -> usize {
        self.rows
    }

    /// Whether the author had a title. The fields behind these two are PRIVATE
    /// on purpose: `pub` fields let any caller write the pair by literal, and a
    /// literal is exactly how the invariant these two express — "refused means
    /// rows = 0" — gets built wrong. Making the constructors `pub(crate)` did
    /// not help and the report that said it did was wrong: visibility of a
    /// function has no bearing on struct-literal syntax, the fields do.
    pub(crate) fn authorized(&self) -> bool {
        self.authorized
    }

    /// The author had a title; the write may or may not have moved a row.
    fn accepted(rows: usize) -> Self {
        Self {
            rows,
            authorized: true,
        }
    }

    /// Nothing happened and nothing was granted. The caller must not stamp the
    /// resource's version slot for this.
    fn refused() -> Self {
        Self {
            rows: 0,
            authorized: false,
        }
    }
}

/// Materializes one replicated registry row. Called from
/// `core_materializer::apply_core_operation` for every TentaVM resource kind.
pub fn apply(
    tx: &rusqlite::Transaction<'_>,
    descriptor: &CoreSyncDescriptor,
    operation: &SyncOperation,
) -> LedgerResult<Applied> {
    let table = registry_table(descriptor.kind)?;
    let columns = table_columns(tx, descriptor.table_name)?;
    let keys = key_columns(descriptor);
    let key_values = read_key_values(operation, &keys)?;

    // The identity a row claims must be the identity its content builds. Without
    // this an operation could carry a made-up `resource_id`, land under it, and
    // then be ordered against — and looked up by — a key nothing else uses.
    let derived = registry_resource_id(&key_values);
    if operation.body.resource_id != derived {
        return Err(SyncLedgerError::Runtime(format!(
            "core sync {} states a resource id that its key columns do not build",
            descriptor.resource_type
        )));
    }

    // An operation that asks for exactly what the row already holds exercises no
    // authority, so it needs none. This is not a convenience: after a baseline
    // reset `reseed_core_state_from_current_rows` restates EVERY row this node
    // knows, most of them owned by other nodes and already agreed on by the
    // receiver. Refusing those would fill every inbox with deferrals that
    // escalate into terminal conflicts about rows nobody was changing — the
    // regression step 5 hit on `sync_nodes` and fixed the same way.
    let changes_nothing =
        operation_changes_nothing(tx, descriptor.table_name, &keys, &key_values, operation)?;

    match authorize(tx, table, descriptor, operation, &keys, &key_values) {
        Ok(Verdict::Write) => {}
        // An ignored operation was not authorized — it was set aside. A stale
        // epoch and a delete of a row this node does not have are both "no
        // title exercised here", so neither may pin the order.
        Ok(Verdict::Ignore) => return Ok(Applied::refused()),
        // The author has no title, and the only reason this is not an error is
        // that the operation asks for nothing. Asking for nothing is not a
        // title either.
        Err(_) if changes_nothing => return Ok(Applied::refused()),
        Err(error) => return Err(error),
    }
    // Authorized, and asking for what is already there. This one DOES stamp:
    // step 5 established that an authorized restatement takes its place in the
    // order, or a later operation from a slower node wins over it.
    if changes_nothing {
        return Ok(Applied::accepted(0));
    }

    check_enum_columns(table, descriptor, operation)?;
    check_host_discriminator(tx, descriptor, operation, &keys, &key_values)?;
    check_append_only(tx, descriptor, operation, &keys, &key_values)?;
    check_derived_id(descriptor, operation)?;
    // The request a decision is about, loaded ONCE and before anything is
    // written: the term is checked against it here, and the grant is projected
    // from it after the row lands.
    let decided_request = if descriptor.kind == Kind::VmAccessDecision {
        Some(request_of_decision(tx, operation)?)
    } else {
        None
    };
    if let Some(request) = &decided_request {
        check_decision_term(request, operation)?;
    }
    let rows = write_row(tx, descriptor, operation, &keys, &key_values, &columns)?;
    // A decision that arrived from the mesh has to move the GRANT it implies,
    // here, in this transaction. The grant is a function of the decision set
    // (`tentavm::access`), and a function nobody evaluates is a value nobody
    // has: without this the rule would hold on the node where somebody clicked
    // and nowhere else, which is precisely the split the append-only design
    // exists to prevent. It is also what lets a rejection arriving AFTER an
    // approval take the grant away — the fold simply returns a different
    // answer and the projection follows it.
    if let (Some(request), true) = (&decided_request, rows > 0) {
        reproject(tx, request)?;
    }
    Ok(Applied::accepted(rows))
}

/// The request row a decision names, or a DEFERRAL when it has not been
/// materialized here yet — it may be behind this operation in the inbox, and
/// applying the decision without it would leave one the fold can never find.
fn request_of_decision(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<crate::tentavm::access::StoredRequest> {
    let request_id = field_string(operation, "request_id")?;
    let instance_id = field_string(operation, "instance_id")?;
    let org_id = field_string(operation, "org_id")?;
    crate::tentavm::access::load(tx, &instance_id, &org_id, &request_id)
        .map_err(|e| SyncLedgerError::Runtime(format!("tentavm access projection: {e}")))?
        .map(|(request, _)| request)
        .ok_or_else(|| {
            SyncLedgerError::DeferredOrdering(format!(
                "core sync decision names request '{request_id}', which is not here yet"
            ))
        })
}

/// W3, the half that only exists on the WRITE side: a decision made after the
/// request's term had passed is REFUSED, on every node.
///
/// The comparison is `decided_at` against `expires_at` — the moment the
/// decision was MADE, not the moment this node happened to apply it. Judging by
/// arrival would refuse a perfectly good decision on a node that was offline for
/// a week, and would accept a stale one on a node that was fast.
///
/// Without this, the handler's own expiry gate protects only the node somebody
/// clicked on: a peer running a patched client could mint a decision on a
/// request forgotten a month ago and every other node would write the grant.
/// Terminal — no later operation makes a passed term unpassed.
fn check_decision_term(
    request: &crate::tentavm::access::StoredRequest,
    operation: &SyncOperation,
) -> LedgerResult<()> {
    let decided_at = field_string(operation, "decided_at")?;
    if decided_at.as_str() > request.expires_at.as_str() {
        return Err(SyncLedgerError::Runtime(format!(
            "core sync decision on request '{}' was made at {decided_at}, after its term \
             ended at {}",
            request.id, request.expires_at
        )));
    }
    Ok(())
}

/// Re-folds the request and brings its computed grant into agreement.
///
/// No capture from here. This node is REPLAYING somebody else's decision, and
/// every other node replays the same one and computes the same grant; minting an
/// operation for it would multiply one decision into N grant writes racing each
/// other in the ledger. The projection is local precisely because the fold is
/// deterministic — that is the whole point of computing the grant rather than
/// sending it.
fn reproject(
    tx: &rusqlite::Transaction<'_>,
    request: &crate::tentavm::access::StoredRequest,
) -> LedgerResult<()> {
    let decisions = crate::tentavm::access::decisions_of(tx, &request.id)
        .map_err(|e| SyncLedgerError::Runtime(format!("tentavm access projection: {e}")))?;
    let local = local_node_id_of(tx)?;
    crate::tentavm::access::project_grants(
        tx,
        request,
        &decisions,
        &crate::tentavm::now_for_registry(),
        &local,
    )
    .map_err(|e| SyncLedgerError::Runtime(format!("tentavm access projection: {e}")))?;
    Ok(())
}

/// Which node this installation is, read inside the apply transaction rather
/// than from the process-global sync runtime — the same reason
/// `core_materializer::local_node_id` gives: a unit test never initializes the
/// global, so a rule written against it silently evaporates.
fn local_node_id_of(tx: &rusqlite::Transaction<'_>) -> LedgerResult<String> {
    let id: Option<String> = tx
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![crate::db::repository::LOCAL_NODE_ID_SETTING],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    Ok(id.unwrap_or_default())
}

/// The two access-request tables are APPEND-ONLY, and this is where that stops
/// being an intention.
///
/// A request row carries no decision fields and a decision row is one
/// administrator's answer, so once either exists there is nothing legitimate
/// left to change: the whole convergence argument (`tentavm::access`) rests on
/// the set of rows only ever growing. Without this the OWNER of a row could
/// still rewrite its `reason` after an administrator had read it, or delete a
/// decision that lost — which is the audit rewriting itself.
///
/// Terminal, not deferrable: no later operation makes a rewrite acceptable.
/// A pure RESTATEMENT never reaches here — `apply` returns on
/// `operation_changes_nothing` above, which is what keeps a baseline reseed
/// from turning every stored request into a conflict.
fn check_append_only(
    tx: &rusqlite::Transaction<'_>,
    descriptor: &CoreSyncDescriptor,
    operation: &SyncOperation,
    keys: &[&str],
    key_values: &[String],
) -> LedgerResult<()> {
    if !matches!(
        descriptor.kind,
        Kind::VmAccessRequest | Kind::VmAccessDecision
    ) {
        return Ok(());
    }
    if row_exists(tx, descriptor.table_name, keys, key_values)? {
        return Err(SyncLedgerError::Runtime(format!(
            "core sync {} is append-only: '{}' exists and may not be rewritten or removed",
            descriptor.resource_type, operation.body.resource_id
        )));
    }
    if operation.body.action == ActionType::Delete {
        return Err(SyncLedgerError::Runtime(format!(
            "core sync {} is append-only: rows are never deleted",
            descriptor.resource_type
        )));
    }
    Ok(())
}

/// The key of an access-request row is a digest of its own CONTENT, so this
/// recomputes it and refuses a row whose id its fields do not build.
///
/// `apply` already checks that the resource id matches the KEY COLUMNS, which
/// stops a row landing under a name nothing else uses. It does not stop a peer
/// choosing the key itself — and for these two tables the key is the identity
/// the whole path is ordered and looked up by, so a made-up one would let a peer
/// file a request "from" somebody else under an id of its own choosing, or
/// attach a decision to a request that does not describe it.
///
/// Terminal: a mismatched digest never becomes correct.
fn check_derived_id(
    descriptor: &CoreSyncDescriptor,
    operation: &SyncOperation,
) -> LedgerResult<()> {
    let expected = match descriptor.kind {
        Kind::VmAccessRequest => crate::tentavm::access::request_id(
            &field_string(operation, "instance_id")?,
            &field_string(operation, "org_id")?,
            &field_string(operation, "requested_by")?,
            &field_string(operation, "scope")?,
            optional_present_string(operation, "host_id")?.as_deref(),
            optional_present_string(operation, "role")?.as_deref(),
            &field_string(operation, "requested_seq")?,
        ),
        Kind::VmAccessDecision => crate::tentavm::access::decision_id(
            &field_string(operation, "request_id")?,
            &field_string(operation, "decided_by")?,
            &field_string(operation, "decided_seq")?,
        ),
        _ => return Ok(()),
    };
    let stated = field_string(operation, "id")?;
    if stated != expected {
        return Err(SyncLedgerError::Runtime(format!(
            "core sync {} states an id its own content does not build",
            descriptor.resource_type
        )));
    }
    Ok(())
}

/// Every key column has to be stated, as a string: the key is what the row IS,
/// and a row that cannot say which one it is has nothing to be applied to.
fn read_key_values(operation: &SyncOperation, keys: &[&str]) -> LedgerResult<Vec<String>> {
    keys.iter()
        .map(|key| field_string(operation, key))
        .collect()
}

fn check_enum_columns(
    table: &RegistryTable,
    descriptor: &CoreSyncDescriptor,
    operation: &SyncOperation,
) -> LedgerResult<()> {
    for (column, allowed) in table.enum_columns {
        let Some(value) = optional_present_string(operation, column)? else {
            continue;
        };
        if !allowed.contains(&value.as_str()) {
            return Err(SyncLedgerError::Runtime(format!(
                "replicated {} has invalid {column}: '{value}'",
                descriptor.resource_type
            )));
        }
    }
    Ok(())
}

/// `vm_hosts` has a CHECK the enum lists cannot express: a `node` host names a
/// node and no connector, a `connector_host` names a connector and no node. It
/// is load-bearing — it is what stops a connector host from claiming a mesh node
/// id — and it has to be evaluated on the row as it will STAND, not on the
/// fields the operation happens to carry, because an update states only some of
/// them.
fn check_host_discriminator(
    tx: &rusqlite::Transaction<'_>,
    descriptor: &CoreSyncDescriptor,
    operation: &SyncOperation,
    keys: &[&str],
    key_values: &[String],
) -> LedgerResult<()> {
    if descriptor.kind != Kind::VmHost || operation.body.action == ActionType::Delete {
        return Ok(());
    }
    let merged = |column: &str| -> LedgerResult<Option<String>> {
        match operation.body.changed_fields.get(column) {
            Some(FieldValue::String(value)) => Ok(Some(value.clone())),
            Some(FieldValue::Null) => Ok(None),
            Some(_) => Err(SyncLedgerError::Runtime(format!(
                "replicated {} has a non-text {column}",
                descriptor.resource_type
            ))),
            None => current_column(tx, descriptor.table_name, keys, key_values, column),
        }
    };
    let kind = merged("kind")?.unwrap_or_default();
    let has_node = merged("node_id")?.is_some();
    let has_connector = merged("connector_id")?.is_some();
    let consistent = match kind.as_str() {
        "node" => has_node && !has_connector,
        "connector_host" => has_connector && !has_node,
        // An unknown kind is refused by `check_enum_columns`; nothing to add.
        _ => true,
    };
    if consistent {
        Ok(())
    } else {
        Err(SyncLedgerError::Runtime(format!(
            "replicated {} of kind '{kind}' names the wrong discriminator",
            descriptor.resource_type
        )))
    }
}

/// One column of the target row as it stands now, or `None` when the column is
/// NULL or the row does not exist yet.
fn current_column(
    tx: &rusqlite::Transaction<'_>,
    table_name: &str,
    keys: &[&str],
    key_values: &[String],
    column: &str,
) -> LedgerResult<Option<String>> {
    let sql = format!(
        "SELECT \"{column}\" FROM {table_name} WHERE {}",
        where_clause(keys)
    );
    let value: Option<Option<String>> = tx
        .query_row(&sql, rusqlite::params_from_iter(key_values), |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()
        .map_err(sql_error)?;
    Ok(value.flatten())
}

fn where_clause(keys: &[&str]) -> String {
    keys.iter()
        .enumerate()
        .map(|(index, key)| format!("\"{key}\" = ?{}", index + 1))
        .collect::<Vec<_>>()
        .join(" AND ")
}

// =============================================================================
// Ownership (plan §6.1)
// =============================================================================

/// Reads the ownership of one row, following `OwnerRule` until it reaches a
/// table that names an owner. `Ok(None)` means the row this rule points at is
/// not here — a missing parent, or a row that does not exist yet.
fn ownership_of(
    tx: &rusqlite::Transaction<'_>,
    kind: Kind,
    keys: &[&str],
    key_values: &[String],
) -> LedgerResult<Option<Ownership>> {
    let table = registry_table(kind)?;
    let descriptor = descriptor_for_kind(kind);
    match table.owner {
        OwnerRule::Own => read_own_ownership(tx, descriptor.table_name, keys, key_values),
        OwnerRule::Parent {
            local_key,
            parent_table,
            ..
        } => {
            let Some(parent_id) =
                current_column(tx, descriptor.table_name, keys, key_values, local_key)?
            else {
                return Ok(None);
            };
            ownership_by_parent_id(tx, parent_table, &parent_id)
        }
        // Nobody owns it, so nobody's `owner_node_id` answers for it.
        OwnerRule::Organization => Ok(None),
    }
}

/// Ownership of the parent row named by `parent_id`, resolved recursively: a
/// disk asks its machine, the machine asks its host, the host answers.
fn ownership_by_parent_id(
    tx: &rusqlite::Transaction<'_>,
    parent_table: &str,
    parent_id: &str,
) -> LedgerResult<Option<Ownership>> {
    let parent_kind = super::core_registry::descriptor_for_table(parent_table)
        .ok_or_else(|| {
            SyncLedgerError::Runtime(format!("unknown TentaVM parent table: {parent_table}"))
        })?
        .kind;
    let parent_descriptor = descriptor_for_kind(parent_kind);
    let parent_keys = key_columns(parent_descriptor);
    // Every parent in this registry is keyed by a single `id` column, so the
    // reference is one value. `parents_are_single_key_tables` keeps that true.
    if parent_keys.len() != 1 {
        return Err(SyncLedgerError::Runtime(format!(
            "TentaVM parent table {parent_table} is not single-keyed"
        )));
    }
    ownership_of(
        tx,
        parent_kind,
        &parent_keys,
        &[parent_id.to_string()],
    )
}

fn read_own_ownership(
    tx: &rusqlite::Transaction<'_>,
    table_name: &str,
    keys: &[&str],
    key_values: &[String],
) -> LedgerResult<Option<Ownership>> {
    let has_epoch = table_columns(tx, table_name)?
        .iter()
        .any(|column| column.name == "owner_epoch");
    let sql = if has_epoch {
        format!(
            "SELECT owner_node_id, owner_epoch FROM {table_name} WHERE {}",
            where_clause(keys)
        )
    } else {
        format!(
            "SELECT owner_node_id, NULL FROM {table_name} WHERE {}",
            where_clause(keys)
        )
    };
    tx.query_row(&sql, rusqlite::params_from_iter(key_values), |row| {
        Ok(Ownership {
            node: row.get(0)?,
            epoch: row.get(1)?,
        })
    })
    .optional()
    .map_err(sql_error)
}

/// Does the target row exist on this node?
fn row_exists(
    tx: &rusqlite::Transaction<'_>,
    table_name: &str,
    keys: &[&str],
    key_values: &[String],
) -> LedgerResult<bool> {
    let sql = format!(
        "SELECT 1 FROM {table_name} WHERE {}",
        where_clause(keys)
    );
    Ok(tx
        .query_row(&sql, rusqlite::params_from_iter(key_values), |_| Ok(true))
        .optional()
        .map_err(sql_error)?
        .unwrap_or(false))
}

/// Plan §6.1, all of it. Decides whether this author may write this row.
///
/// A refusal is DEFERRABLE whenever the operation that would make it legal can
/// still be behind this one in the inbox (the host it belongs to, the machine it
/// hangs off, the epoch it skips). It is terminal when no later operation could
/// make it legal — a first insert naming somebody else as owner never becomes
/// acceptable, however long it waits.
fn authorize(
    tx: &rusqlite::Transaction<'_>,
    table: &RegistryTable,
    descriptor: &CoreSyncDescriptor,
    operation: &SyncOperation,
    keys: &[&str],
    key_values: &[String],
) -> LedgerResult<Verdict> {
    let actor = operation.body.actor_node_id.as_str();
    let exists = row_exists(tx, descriptor.table_name, keys, key_values)?;

    if !exists {
        match operation.body.action {
            // Nothing to remove, so there is nothing to authorize: the row's
            // absence is not a fact a peer's word should create. The
            // delete-before-insert race this gives up does not exist — the
            // ledger delivers one author's operations in order, and two authors
            // never both own the same row.
            //
            // `Ignore` also means "no title was exercised here", so the caller
            // does not stamp the resource's LWW slot for it. That matters: the
            // resource id of a host of kind `node` is the public node id, so
            // without this a stranger pinned the order of a row that did not
            // exist yet and the owner's later insert lost to it. What remains
            // for step 17 is narrower and unreachable by any ownership rule:
            // the row's TRUE owner can still pin its own slot with a clock from
            // the future.
            ActionType::Delete => return Ok(Verdict::Ignore),
            // The row this update edits has not been materialized yet.
            ActionType::Update => {
                return Err(SyncLedgerError::DeferredOrdering(format!(
                    "core sync target row not found: {}/{}",
                    descriptor.resource_type, operation.body.resource_id
                )))
            }
            ActionType::Insert => return authorize_first_insert(tx, table, descriptor, operation),
        }
    }

    // The row is here, so an Insert is an Update (plan §6.1): whoever owns what
    // is already there decides, and an upsert cannot take a row over.
    match table.owner {
        OwnerRule::Organization => authorize_organization(tx, actor, descriptor),
        OwnerRule::Own => {
            let current = read_own_ownership(tx, descriptor.table_name, keys, key_values)?
                .ok_or_else(|| {
                    SyncLedgerError::Runtime(format!(
                        "core sync row of {} has no owner",
                        descriptor.resource_type
                    ))
                })?;
            authorize_against_owner(operation, descriptor, &current, true)
        }
        OwnerRule::Parent {
            local_key,
            parent_table,
            nullable,
        } => {
            // Both ends have to answer to the same node: the parent the row hangs
            // off today, and the parent the operation moves it to. Otherwise the
            // owner of an empty machine could pull somebody else's disk onto it.
            let held = ownership_of(tx, descriptor.kind, keys, key_values)?;
            let stated = match optional_present_string(operation, local_key)? {
                Some(parent_id) => ownership_by_parent_id(tx, parent_table, &parent_id)?,
                None => held.clone(),
            };
            for (side, ownership) in [("current", &held), ("stated", &stated)] {
                match ownership {
                    Some(ownership) => {
                        // A satellite carries an epoch only where the plan gives it
                        // one (`vm_connector_secret_grants`), and then it is the
                        // PARENT's epoch: an envelope sealed before a connector
                        // changed hands is stale, not authoritative.
                        if let Verdict::Ignore =
                            authorize_against_owner(operation, descriptor, ownership, false)?
                        {
                            return Ok(Verdict::Ignore);
                        }
                    }
                    None if nullable => {
                        authorize_organization(tx, actor, descriptor)?;
                    }
                    None => {
                        // Deleting the leftovers of a machine that is already gone
                        // is the last thing anybody wants to block; the row is an
                        // orphan either way. But "do not block it" is not "let
                        // anybody do it": with no parent there is no owner to
                        // compare against, so the author has to hold the one
                        // title that does not depend on a parent. Without this
                        // check the rule read the actor NOT ONCE, and `guest_id`
                        // has no foreign key, so orphans are reachable.
                        if operation.body.action == ActionType::Delete {
                            return authorize_organization(tx, actor, descriptor);
                        }
                        return Err(SyncLedgerError::DeferredOrdering(format!(
                            "core sync {} has no {side} parent in {parent_table}",
                            descriptor.resource_type
                        )));
                    }
                }
            }
            Ok(Verdict::Write)
        }
    }
}

/// Plan §6.1, first insert: the author has to be the owner it names, the epoch
/// has to start at zero, and a host of kind `node` has to be the node writing
/// it. All three are terminal — no later operation makes them true.
fn authorize_first_insert(
    tx: &rusqlite::Transaction<'_>,
    table: &RegistryTable,
    descriptor: &CoreSyncDescriptor,
    operation: &SyncOperation,
) -> LedgerResult<Verdict> {
    let actor = operation.body.actor_node_id.as_str();
    match table.owner {
        OwnerRule::Organization => authorize_organization(tx, actor, descriptor),
        OwnerRule::Own => {
            let owner = field_string(operation, "owner_node_id")?;
            if owner != actor {
                return Err(SyncLedgerError::Runtime(format!(
                    "node '{actor}' may not create a {} owned by '{owner}'",
                    descriptor.resource_type
                )));
            }
            if optional_present_i64(operation, "owner_epoch")?.unwrap_or(0) != 0 {
                return Err(SyncLedgerError::Runtime(format!(
                    "a new {} must start at owner epoch 0",
                    descriptor.resource_type
                )));
            }
            // A `node` host IS a mesh node: its id and its `node_id` are that
            // node's id (plan §4.1). Letting one node publish another node's host
            // row would put a machine list under an identity its author does not
            // hold, and the partial unique index would then refuse the real one.
            if descriptor.kind == Kind::VmHost
                && optional_present_string(operation, "kind")?.as_deref() == Some("node")
            {
                let node_id = field_string(operation, "node_id")?;
                if node_id != actor {
                    return Err(SyncLedgerError::Runtime(format!(
                        "node '{actor}' may not publish the host row of node '{node_id}'"
                    )));
                }
                // And its id IS that node id (plan §4.1) — minting a second
                // identifier would only add a mapping nothing can resolve from a
                // job or a grant. Said here rather than left to the partial
                // unique index, which would answer with raw constraint text and
                // only once the second row arrived.
                let id = field_string(operation, "id")?;
                if id != node_id {
                    return Err(SyncLedgerError::Runtime(format!(
                        "the host row of node '{node_id}' must be keyed by that node id, not '{id}'"
                    )));
                }
            }
            Ok(Verdict::Write)
        }
        OwnerRule::Parent {
            local_key,
            parent_table,
            nullable,
        } => {
            let parent_id = match optional_present_string(operation, local_key)? {
                Some(parent_id) => parent_id,
                None if nullable => return authorize_organization(tx, actor, descriptor),
                None => {
                    return Err(SyncLedgerError::Runtime(format!(
                        "a new {} must name its {local_key}",
                        descriptor.resource_type
                    )))
                }
            };
            match ownership_by_parent_id(tx, parent_table, &parent_id)? {
                Some(owner) => authorize_against_owner(operation, descriptor, &owner, false),
                // Plan §6.1: "no parent = rejection". Deferrable, because the
                // insert that creates the parent may be behind this one.
                None => Err(SyncLedgerError::DeferredOrdering(format!(
                    "core sync {} names a {local_key} that is not in {parent_table} yet",
                    descriptor.resource_type
                ))),
            }
        }
    }
}

/// The only place that compares an author against an owner and an epoch.
///
/// `transferable` is true for the two tables that carry `owner_epoch` on the row
/// itself. Everywhere else the epoch belongs to the parent and the row may only
/// restate it.
fn authorize_against_owner(
    operation: &SyncOperation,
    descriptor: &CoreSyncDescriptor,
    current: &Ownership,
    transferable: bool,
) -> LedgerResult<Verdict> {
    let actor = operation.body.actor_node_id.as_str();
    let held_epoch = current.epoch.unwrap_or(0);
    let stated_epoch = optional_present_i64(operation, "owner_epoch")?;

    if let Some(stated) = stated_epoch {
        if stated < held_epoch {
            // Plan §6.1: "stara epoka = ignorowana". Not an error: an operation
            // minted before a transfer is simply about a past the row has left.
            return Ok(Verdict::Ignore);
        }
        if stated > held_epoch + 1 || (stated == held_epoch + 1 && !transferable) {
            return Err(SyncLedgerError::DeferredOrdering(format!(
                "core sync {} states owner epoch {stated} over {held_epoch}",
                descriptor.resource_type
            )));
        }
    }
    // Whether it is an ordinary write or the epoch bump that hands the row over,
    // the author must be the node that owns it TODAY. Plan §6.1 also allows the
    // target of a valid transfer to write the bump — phase 0 has no transfer
    // record to check that against (no `ConnectorTransferOwnerRequest`, no jti
    // table, §6.2 is phase 1), and accepting an epoch bump from anybody would BE
    // the takeover the epoch exists to prevent. So the arm that exists is the one
    // §6.1 describes for host migration: the source writes `switch_owner` after
    // the target confirms.
    if actor != current.node {
        return Err(SyncLedgerError::Runtime(format!(
            "node '{actor}' may not write a {} owned by '{}'",
            descriptor.resource_type, current.node
        )));
    }
    // Ownership moves with the epoch or not at all: without this an owner could
    // hand the row to a node that never agreed to take it, and the receiver would
    // have no way to tell that from the transfer the plan describes.
    if let Some(new_owner) = optional_present_string(operation, "owner_node_id")? {
        if new_owner != current.node && stated_epoch != Some(held_epoch + 1) {
            return Err(SyncLedgerError::Runtime(format!(
                "core sync {} moves ownership to '{new_owner}' without bumping the epoch",
                descriptor.resource_type
            )));
        }
    }
    Ok(Verdict::Write)
}

fn authorize_organization(
    tx: &rusqlite::Transaction<'_>,
    actor: &str,
    descriptor: &CoreSyncDescriptor,
) -> LedgerResult<Verdict> {
    if super::core_materializer::node_is_operator(tx, actor)? {
        Ok(Verdict::Write)
    } else {
        // Deferrable for the same reason as `sync_nodes`: the operation that puts
        // the author on the operator list may still be queued behind this one.
        Err(SyncLedgerError::DeferredOrdering(format!(
            "node '{actor}' may not write {}: it is not on the operator list",
            descriptor.resource_type
        )))
    }
}

// =============================================================================
// Writing the row
// =============================================================================

/// Writes the columns the operation states, and only those.
///
/// Presence is the statement: a column the operation does not name keeps the
/// value the row has, and takes the schema default on a genuinely new row. The
/// alternative — substituting a default for an unnamed column — is what turned
/// an `Insert` about one field into a silent reset of every other one on
/// `sync_nodes`, and it would do worse here, where the unnamed columns include
/// `owner_node_id`.
fn write_row(
    tx: &rusqlite::Transaction<'_>,
    descriptor: &CoreSyncDescriptor,
    operation: &SyncOperation,
    keys: &[&str],
    key_values: &[String],
    columns: &[ColumnInfo],
) -> LedgerResult<usize> {
    let table_name = descriptor.table_name;
    if operation.body.action == ActionType::Delete {
        let sql = format!("DELETE FROM {table_name} WHERE {}", where_clause(keys));
        return tx
            .execute(&sql, rusqlite::params_from_iter(key_values))
            .map_err(sql_error);
    }

    // The ledger envelope adds `capture_id` to every operation's fields
    // (`runtime::build_core_operation`), and a peer on a newer schema states
    // columns this node does not have yet. Neither is a data error: taking the
    // intersection with the real schema is what lets a fleet upgrade one node at
    // a time, and it is why this walks the COLUMNS rather than the fields.
    let stated: Vec<&ColumnInfo> = columns
        .iter()
        .filter(|column| operation.body.changed_fields.contains_key(&column.name))
        .collect();

    let exists = row_exists(tx, table_name, keys, key_values)?;
    if !exists {
        // A row that is being created has to state everything the schema needs.
        // Letting SQLite discover it would turn a truncated operation into a
        // terminal conflict carrying raw NOT NULL text; saying it here names the
        // column.
        let missing: Vec<&str> = columns
            .iter()
            .filter(|column| {
                column.not_null
                    && !column.has_default
                    && !operation.body.changed_fields.contains_key(&column.name)
            })
            .map(|column| column.name.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(SyncLedgerError::Runtime(format!(
                "core sync {} does not state required columns: {}",
                descriptor.resource_type,
                missing.join(", ")
            )));
        }
    }

    if operation.body.action == ActionType::Update && !exists {
        return Err(SyncLedgerError::DeferredOrdering(format!(
            "core sync target row not found: {}/{}",
            descriptor.resource_type, operation.body.resource_id
        )));
    }

    let assignments: Vec<&&ColumnInfo> = stated
        .iter()
        .filter(|column| !keys.contains(&column.name.as_str()))
        .collect();
    let value_of = |column: &ColumnInfo| -> LedgerResult<rusqlite::types::Value> {
        field_to_sql(
            operation
                .body
                .changed_fields
                .get(&column.name)
                .expect("column was selected because the operation states it"),
        )
    };

    // Plan §6.1: an Insert on an existing key IS an update. Writing it as one
    // rather than as an upsert is not a style choice — an upsert re-states the
    // whole row to SQLite, so an operation that names three columns of a row
    // that already has fifteen would fail NOT NULL on the twelve it never meant
    // to touch.
    if exists {
        if assignments.is_empty() {
            return Ok(0);
        }
        let sets = assignments
            .iter()
            .enumerate()
            .map(|(index, column)| format!("\"{}\" = ?{}", column.name, index + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let key_placeholders = keys
            .iter()
            .enumerate()
            .map(|(index, key)| format!("\"{key}\" = ?{}", assignments.len() + index + 1))
            .collect::<Vec<_>>()
            .join(" AND ");
        let sql = format!("UPDATE {table_name} SET {sets} WHERE {key_placeholders}");
        let mut params = assignments
            .iter()
            .map(|column| value_of(column))
            .collect::<LedgerResult<Vec<_>>>()?;
        params.extend(
            key_values
                .iter()
                .map(|value| rusqlite::types::Value::Text(value.clone())),
        );
        return tx
            .execute(&sql, rusqlite::params_from_iter(params))
            .map_err(sql_error);
    }

    let names = stated
        .iter()
        .map(|column| format!("\"{}\"", column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=stated.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("INSERT INTO {table_name} ({names}) VALUES ({placeholders})");
    let params = stated
        .iter()
        .map(|column| value_of(column))
        .collect::<LedgerResult<Vec<_>>>()?;
    tx.execute(&sql, rusqlite::params_from_iter(params))
        .map_err(sql_error)
}

/// Ledger value to SQLite value. `Bytes` is refused rather than stored: plan
/// §4.1 keeps this registry free of BLOB columns precisely so that ciphertext
/// travels as base64 text, and a peer sending bytes is describing a schema this
/// one does not have.
fn field_to_sql(value: &FieldValue) -> LedgerResult<rusqlite::types::Value> {
    use rusqlite::types::Value;
    Ok(match value {
        FieldValue::Null => Value::Null,
        FieldValue::Bool(value) => Value::Integer(i64::from(*value)),
        FieldValue::I64(value) => Value::Integer(*value),
        FieldValue::U64(value) => Value::Integer(i64::try_from(*value).map_err(|e| {
            SyncLedgerError::Runtime(format!("TentaVM registry field out of range: {e}"))
        })?),
        FieldValue::Decimal(value) | FieldValue::String(value) => Value::Text(value.clone()),
        FieldValue::Bytes(_) => {
            return Err(SyncLedgerError::Runtime(
                "TentaVM registry rows carry no binary fields".to_string(),
            ))
        }
    })
}

// =============================================================================
// Capture — a local write leaving for the mesh
// =============================================================================

/// Ledger scope of every TentaVM capture: the DEFAULT organization, not the
/// row's own `org_id`. `ensure_default_core_sync_policies` seeds policies under
/// the default org only, so a capture minted anywhere else would resolve to zero
/// targets and sit in the outbox forever. The row's real `org_id` travels as a
/// field, and that is what the receiver writes — the same arrangement Code
/// Studio, Flow Builder and RBAC use.
const CAPTURE_ORG_ID: &str = crate::services::org::DEFAULT_ORG_ID;

/// Captures the CURRENT state of one registry row, keyed by `key_values` in the
/// descriptor's key order. A row that is present is captured as an Insert of
/// every column; a row that is gone is captured as a Delete tombstone, so a
/// removal replicates instead of being undone by an older insert still in
/// flight.
///
/// Reading the row back rather than taking the caller's fields is deliberate,
/// and it is the reason the arm and the capture cannot drift: there is exactly
/// one description of a registry row on the wire, and it is the row.
pub fn capture_row(
    tx: &rusqlite::Transaction<'_>,
    kind: Kind,
    key_values: &[&str],
) -> anyhow::Result<()> {
    let descriptor = descriptor_for_kind(kind);
    let keys = key_columns(descriptor);
    if keys.len() != key_values.len() {
        anyhow::bail!(
            "tentavm capture of {}: {} key values for {} key columns",
            descriptor.resource_type,
            key_values.len(),
            keys.len()
        );
    }
    let columns = table_columns(tx, descriptor.table_name)
        .map_err(|e| anyhow::anyhow!("tentavm capture: {e}"))?;
    let names = columns
        .iter()
        .map(|column| format!("\"{}\"", column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {names} FROM {} WHERE {}",
        descriptor.table_name,
        where_clause(&keys)
    );
    let row = tx
        .query_row(&sql, rusqlite::params_from_iter(key_values), |row| {
            let mut fields = std::collections::BTreeMap::new();
            for (index, column) in columns.iter().enumerate() {
                fields.insert(column.name.clone(), sql_to_field(row.get_ref(index)?)?);
            }
            Ok(fields)
        })
        .optional()?;

    let owned: Vec<String> = key_values.iter().map(|value| value.to_string()).collect();
    let (action, fields) = match row {
        Some(fields) => (crate::sync::runtime::SqlWriteAction::Insert, fields),
        None => {
            // A tombstone still has to say which row it is about, so the key
            // columns travel even when nothing else is left to state.
            let mut fields = std::collections::BTreeMap::new();
            for (column, value) in keys.iter().zip(owned.iter()) {
                fields.insert((*column).to_string(), FieldValue::String(value.clone()));
            }
            (crate::sync::runtime::SqlWriteAction::Delete, fields)
        }
    };
    crate::db::repository::record_core_capture_for_org_tx(
        tx,
        kind,
        CAPTURE_ORG_ID,
        registry_resource_id(&owned),
        action,
        fields,
        // No acting user is bound to the capture: this registry has no FK to
        // `user_accounts` (a replicated row may name an account a node has not
        // materialized), and the journal does have one. Who acted is in
        // `audit_log`, and `granted_by` / `created_by` travel in the fields.
        None,
    )?;
    Ok(())
}

fn sql_to_field(value: rusqlite::types::ValueRef<'_>) -> rusqlite::Result<FieldValue> {
    use rusqlite::types::ValueRef;
    Ok(match value {
        ValueRef::Null => FieldValue::Null,
        ValueRef::Integer(value) => FieldValue::I64(value),
        // Plan §4.1 has no REAL column in this registry; if one ever appears it
        // must be stated deliberately, not silently rounded through a decimal.
        ValueRef::Real(value) => FieldValue::Decimal(value.to_string()),
        ValueRef::Text(bytes) => FieldValue::String(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(_) => {
            return Err(rusqlite::Error::InvalidColumnType(
                0,
                "tentavm registry column is a blob".to_string(),
                rusqlite::types::Type::Blob,
            ))
        }
    })
}

/// Re-states every row of one registry table as a capture, for
/// `reseed_core_state_from_current_rows`. It goes through `capture_row`, so a
/// re-seeded row is byte-for-byte the row a live write would have sent.
pub fn reseed_table(tx: &rusqlite::Transaction<'_>, kind: Kind) -> anyhow::Result<usize> {
    let descriptor = descriptor_for_kind(kind);
    let keys = key_columns(descriptor);
    let names = keys
        .iter()
        .map(|key| format!("\"{key}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT {names} FROM {}", descriptor.table_name);
    let rows = {
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt
            .query_map([], |row| {
                (0..keys.len())
                    .map(|index| row.get::<_, String>(index))
                    .collect::<rusqlite::Result<Vec<String>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut emitted = 0usize;
    for values in rows {
        let borrowed: Vec<&str> = values.iter().map(String::as_str).collect();
        capture_row(tx, kind, &borrowed)?;
        emitted += 1;
    }
    Ok(emitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbPool;
    use crate::services::org::DEFAULT_ORG_ID;
    use crate::sync::core_materializer::apply_core_operation;
    use crate::sync::ledger::{
        ActionType, BaselineEpoch, HybridLogicalTimestamp, OperationId, PartitionId,
        SyncOperationBody,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn make_db() -> DbPool {
        crate::db::init(std::path::Path::new(":memory:")).expect("db")
    }

    fn cipher() -> Arc<crate::crypto::SettingsCipher> {
        Arc::new(crate::crypto::SettingsCipher::new(&[9u8; 32]))
    }

    fn text(value: &str) -> FieldValue {
        FieldValue::String(value.to_string())
    }

    fn int(value: i64) -> FieldValue {
        FieldValue::I64(value)
    }

    /// A core-sync operation about one TentaVM row, shaped the way
    /// `runtime::build_core_operation` shapes it — `capture_id` included, because
    /// that field rides on every real operation and is a column of no table.
    ///
    /// `wall` matters: these resources are LWW-ordered, so two operations about
    /// one row need distinct clocks or the second is (correctly) dropped as
    /// stale, and a test that forgot would be reading its own silence.
    fn operation(
        kind: Kind,
        actor: &str,
        action: ActionType,
        wall: i64,
        fields: &[(&str, FieldValue)],
    ) -> SyncOperation {
        let descriptor = descriptor_for_kind(kind);
        let mut changed_fields: BTreeMap<String, FieldValue> = fields
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect();
        changed_fields.insert("capture_id".to_string(), text("capture-under-test"));
        let key_values: Vec<String> = key_columns(descriptor)
            .iter()
            .map(|key| match changed_fields.get(*key) {
                Some(FieldValue::String(value)) => value.clone(),
                _ => String::new(),
            })
            .collect();
        SyncOperation {
            op_id: OperationId::from_hash([3; 32]),
            operation_hash: [3; 32],
            body: SyncOperationBody {
                org_id: DEFAULT_ORG_ID.to_string(),
                partition_id: PartitionId::new("core/org/org-default/tentavm").unwrap(),
                node_seq: 1,
                addon_id: super::super::core_registry::CORE_SYNC_ADDON_ID.to_string(),
                resource_type: descriptor.resource_type.to_string(),
                resource_id: registry_resource_id(&key_values),
                table_name: descriptor.table_name.to_string(),
                primary_key: descriptor.primary_key_column.to_string(),
                action,
                changed_fields,
                before_hash: None,
                after_hash: None,
                actor_user_id: String::new(),
                actor_device_id: actor.to_string(),
                actor_node_id: actor.to_string(),
                hlc_timestamp: HybridLogicalTimestamp {
                    wall_time_ms: wall,
                    logical: 0,
                    node_id: actor.to_string(),
                },
                epoch: BaselineEpoch::default(),
                prev_node_hash: None,
                payload_hash: [0; 32],
                acl_snapshot_hash: [0; 32],
                policy_epoch: 0,
                encryption_info: None,
            },
            signature: Vec::new(),
        }
    }

    /// The full field set of a `node` host owned by `owner`, as the capture of a
    /// real row would state it.
    fn host_fields(id: &str, owner: &str) -> Vec<(&'static str, FieldValue)> {
        vec![
            ("id", text(id)),
            ("org_id", text(DEFAULT_ORG_ID)),
            ("kind", text("node")),
            ("node_id", text(owner)),
            ("connector_id", FieldValue::Null),
            ("external_ref", FieldValue::Null),
            ("display_name", text("Host")),
            ("engines_json", text("[]")),
            ("capabilities_json", text("{}")),
            ("status", text("ready")),
            ("owner_node_id", text(owner)),
            ("owner_epoch", FieldValue::I64(0)),
            ("created_at", text("2026-09-05T00:00:00Z")),
            ("updated_at", text("2026-09-05T00:00:00Z")),
            ("updated_by_node", text(owner)),
        ]
    }

    fn connector_fields(id: &str, owner: &str) -> Vec<(&'static str, FieldValue)> {
        vec![
            ("id", text(id)),
            ("org_id", text(DEFAULT_ORG_ID)),
            ("kind", text("proxmox")),
            ("endpoint", text("https://pve.invalid")),
            ("display_name", text("PVE")),
            ("tls_mode", text("verify")),
            ("auth_kind", text("token")),
            ("status", text("unknown")),
            ("owner_node_id", text(owner)),
            ("owner_epoch", FieldValue::I64(0)),
            ("created_at", text("2026-09-05T00:00:00Z")),
            ("updated_at", text("2026-09-05T00:00:00Z")),
            ("updated_by_node", text(owner)),
        ]
    }

    fn guest_fields(id: &str, host_id: &str) -> Vec<(&'static str, FieldValue)> {
        vec![
            ("id", text(id)),
            ("instance_id", text("env-1")),
            ("org_id", text(DEFAULT_ORG_ID)),
            ("host_id", text(host_id)),
            ("kind", text("vm")),
            ("engine", text("kvm")),
            ("name", text("web-01")),
            ("spec_json", text("{}")),
            ("desired_state", text("running")),
            ("observed_state", text("running")),
            ("owner_user_id", text("u-1")),
            ("created_at", text("2026-09-05T00:00:00Z")),
            ("updated_at", text("2026-09-05T00:00:00Z")),
            ("updated_by_node", text("node-a")),
        ]
    }

    fn apply(db: &DbPool, operation: &SyncOperation) -> LedgerResult<usize> {
        apply_core_operation(db, &cipher(), operation)
    }

    fn seed_host(db: &DbPool, owner: &str, wall: i64) {
        let rows = seed_host_at_any_clock(db, owner, wall);
        assert_eq!(rows, 1, "the owner publishes its own host row");
    }

    /// The same insert, without asserting that it won the LWW order — for the
    /// one test that is about losing it.
    fn seed_host_at_any_clock(db: &DbPool, owner: &str, wall: i64) -> usize {
        apply(
            db,
            &operation(Kind::VmHost, owner, ActionType::Insert, wall, &host_fields(owner, owner)),
        )
        .expect("the owner publishes its own host row")
    }

    fn make_operator(db: &DbPool, node_id: &str) {
        let conn = db.write().expect("lock");
        conn.execute(
            "INSERT INTO sync_nodes (node_id, public_key, operator) VALUES (?1, 'pk', 1) \
             ON CONFLICT(node_id) DO UPDATE SET operator = 1",
            rusqlite::params![node_id],
        )
        .expect("operator row");
    }

    fn scalar(db: &DbPool, sql: &str) -> Option<String> {
        let conn = db.read().expect("lock");
        conn.query_row(sql, [], |row| row.get::<_, Option<String>>(0))
            .optional()
            .expect("query")
            .flatten()
    }

    // ---------------------------------------------------------------------
    // The registry against the schema
    // ---------------------------------------------------------------------

    /// Every `vm_*` table the main database has must be part of this registry:
    /// a descriptor (so it may travel), an owner rule (so it may not travel
    /// unowned) and a reseed (which the exhaustive match in
    /// `reseed_core_state_from_current_rows` gets for free).
    ///
    /// This is the guard against the failure this project has already had twice:
    /// a table added by a later migration that silently never replicates, and
    /// nobody notices because nothing asked.
    #[test]
    fn every_vm_table_in_the_main_database_is_in_the_registry() {
        let db = make_db();
        let conn = db.read().expect("lock");
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'vm\\_%' \
                 ESCAPE '\\' ORDER BY name",
            )
            .expect("prepare");
        let tables: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows");
        // Eighteen from plan §4.1, plus the two step 6 added for "Poproś
        // administratora": `vm_access_requests` and `vm_access_decisions`.
        assert_eq!(tables.len(), 20, "the TentaVM registry is 20 tables: {tables:?}");
        for table in &tables {
            let descriptor = super::super::core_registry::descriptor_for_table(table)
                .unwrap_or_else(|| panic!("{table} has no core sync descriptor"));
            assert_eq!(descriptor.partition_suffix, "tentavm");
            assert_eq!(descriptor.scope, super::super::core_registry::CoreSyncScope::Organization);
            assert!(
                table_for_kind(descriptor.kind).is_some(),
                "{table} has a descriptor but no ownership rule"
            );
        }
        assert_eq!(TENTAVM_REGISTRY_TABLES.len(), tables.len());
    }

    /// The descriptor's key is the row's replicated identity, and `apply` builds
    /// both the resource id and the WHERE clause from it. If it disagreed with
    /// the PRIMARY KEY the migration created, two different rows would converge
    /// onto one resource — or one row onto none.
    #[test]
    fn registry_primary_keys_match_the_schema() {
        let db = make_db();
        let conn = db.read().expect("lock");
        for table in TENTAVM_REGISTRY_TABLES {
            let descriptor = descriptor_for_kind(table.kind);
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({})", descriptor.table_name))
                .expect("prepare");
            let mut pk: Vec<(i64, String)> = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(5)?, row.get::<_, String>(1)?))
                })
                .expect("query")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("rows")
                .into_iter()
                .filter(|(position, _)| *position > 0)
                .collect();
            pk.sort_by_key(|(position, _)| *position);
            let declared: Vec<String> = key_columns(descriptor)
                .iter()
                .map(|key| (*key).to_string())
                .collect();
            let actual: Vec<String> = pk.into_iter().map(|(_, name)| name).collect();
            assert_eq!(
                declared, actual,
                "{} declares a key the schema does not have",
                descriptor.table_name
            );
        }
    }

    /// The enum lists are a readable copy of the DDL's CHECK constraints, kept
    /// so a forbidden value is refused by name instead of by raw SQL text. This
    /// is what stops the copy from becoming a second, drifting truth — in both
    /// directions: a value the DDL drops and a CHECK nobody copied.
    #[test]
    fn declared_enums_match_the_schema() {
        let db = make_db();
        let conn = db.read().expect("lock");
        for table in TENTAVM_REGISTRY_TABLES {
            let descriptor = descriptor_for_kind(table.kind);
            let sql: String = conn
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    rusqlite::params![descriptor.table_name],
                    |row| row.get(0),
                )
                .expect("table sql");
            for (column, allowed) in table.enum_columns {
                let rendered = allowed
                    .iter()
                    .map(|value| format!("'{value}'"))
                    .collect::<Vec<_>>()
                    .join(",");
                assert!(
                    sql.contains(&format!("CHECK({column} IN ({rendered}))")),
                    "{}.{column} is not the set the schema checks",
                    descriptor.table_name
                );
            }
            assert_eq!(
                sql.matches(" IN (").count(),
                table.enum_columns.len(),
                "{} has a CHECK ... IN (...) nobody declared",
                descriptor.table_name
            );
        }
    }

    /// A satellite names its parent with one value, so a parent keyed by more
    /// than one column could not be resolved from it.
    #[test]
    fn parents_are_single_key_tables() {
        for table in TENTAVM_REGISTRY_TABLES {
            if let OwnerRule::Parent { parent_table, .. } = table.owner {
                let parent = super::super::core_registry::descriptor_for_table(parent_table)
                    .unwrap_or_else(|| panic!("unknown parent table {parent_table}"));
                assert_eq!(key_columns(parent).len(), 1, "{parent_table} is not single-keyed");
            }
        }
    }

    /// Plan §4.1: no BLOB anywhere in this registry, because the materializer has
    /// no `FieldValue::Bytes` and sealed envelopes travel as base64 text. A BLOB
    /// column would be a row that can never replicate.
    #[test]
    fn no_registry_column_is_a_blob() {
        let db = make_db();
        let conn = db.read().expect("lock");
        for table in TENTAVM_REGISTRY_TABLES {
            let descriptor = descriptor_for_kind(table.kind);
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({})", descriptor.table_name))
                .expect("prepare");
            let types: Vec<(String, String)> = stmt
                .query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
                .expect("query")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("rows");
            for (name, kind) in types {
                assert!(
                    !kind.eq_ignore_ascii_case("BLOB"),
                    "{}.{name} is a BLOB and could never replicate",
                    descriptor.table_name
                );
            }
        }
    }

    // ---------------------------------------------------------------------
    // Capture
    // ---------------------------------------------------------------------

    /// A capture states the row, all of it, read back from the table. That is
    /// what makes the capacity columns of migration 141 replicate without a line
    /// of code naming them — and what makes the next column do the same.
    #[test]
    fn a_capture_states_every_column_of_the_row() {
        let db = make_db();
        seed_host(&db, "node-a", 1_000);
        {
            let conn = db.write().expect("lock");
            conn.execute(
                "UPDATE vm_hosts SET cpu_cores = 16, ram_bytes = 68719476736, \
                 storage_bytes = 2199023255552 WHERE id = 'node-a'",
                [],
            )
            .expect("capacity");
        }
        let (fields, action) = {
            let mut conn = crate::db::repository::acquire_for_baseline(&db).expect("conn");
            let tx = conn.transaction().expect("tx");
            capture_row(&tx, Kind::VmHost, &["node-a"]).expect("capture");
            let (capture_id, action): (String, String) = tx
                .query_row(
                    "SELECT capture_id, action FROM __tentaflow_core_sync_captures \
                     WHERE resource_type = 'core.vm_host' ORDER BY created_at_ms DESC LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("capture row");
            let capture = crate::sync::core_capture::load_core_write_capture(&tx, &capture_id)
                .expect("load")
                .expect("capture");
            tx.commit().expect("commit");
            (capture.changed_fields, action)
        };
        assert_eq!(action, "insert");

        let conn = db.read().expect("lock");
        let mut stmt = conn.prepare("PRAGMA table_info(vm_hosts)").expect("prepare");
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows");
        let stated: std::collections::BTreeSet<&String> = fields.keys().collect();
        for column in &columns {
            assert!(stated.contains(column), "capture does not state {column}");
        }
        assert_eq!(stated.len(), columns.len(), "capture states a field that is no column");
        assert_eq!(fields.get("cpu_cores"), Some(&FieldValue::I64(16)));
        assert_eq!(fields.get("ram_bytes"), Some(&FieldValue::I64(68_719_476_736)));
        assert_eq!(
            fields.get("storage_bytes"),
            Some(&FieldValue::I64(2_199_023_255_552))
        );
    }

    /// A row that is gone is captured as a tombstone that still says which row
    /// it was, so the removal replicates instead of being undone by an older
    /// insert still travelling.
    #[test]
    fn a_missing_row_is_captured_as_a_tombstone() {
        let db = make_db();
        let mut conn = crate::db::repository::acquire_for_baseline(&db).expect("conn");
        let tx = conn.transaction().expect("tx");
        capture_row(&tx, Kind::VmTag, &["env-1", "prod"]).expect("capture");
        let (action, capture_id): (String, String) = tx
            .query_row(
                "SELECT action, capture_id FROM __tentaflow_core_sync_captures \
                 WHERE resource_type = 'core.vm_tag'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("capture");
        assert_eq!(action, "delete");
        let capture = crate::sync::core_capture::load_core_write_capture(&tx, &capture_id)
            .expect("load")
            .expect("capture");
        assert_eq!(capture.changed_fields.get("instance_id"), Some(&text("env-1")));
        assert_eq!(capture.changed_fields.get("name"), Some(&text("prod")));
        assert_eq!(
            capture.resource_id,
            crate::sync::resource_id::composite_resource_id(&["env-1", "prod"])
        );
    }

    // ---------------------------------------------------------------------
    // Ownership — plan §6.1
    // ---------------------------------------------------------------------

    /// Plan §15, "pierwszy insert z cudzym właścicielem → odrzucony". Terminal:
    /// no later operation makes an author into an owner it never was.
    ///
    /// Two tables, and the second one is the point. On a `node` host the
    /// foreign-owner rule is shadowed by the node-id rule, which refuses the
    /// same operation for its own reason — a mutation removing the owner check
    /// left this test green until it also asserted WHICH rule spoke and asked
    /// the same question of a table that has no second rule.
    #[test]
    fn a_first_insert_naming_another_owner_is_refused() {
        let db = make_db();
        let refused = apply(
            &db,
            &operation(
                Kind::VmHost,
                "node-b",
                ActionType::Insert,
                1_000,
                &host_fields("node-a", "node-a"),
            ),
        )
        .expect_err("a stranger may not create somebody else's host row");
        assert!(
            matches!(refused, SyncLedgerError::Runtime(_)),
            "creating a foreign row can never become legal: {refused:?}"
        );
        assert_eq!(scalar(&db, "SELECT id FROM vm_hosts"), None);

        let connector = apply(
            &db,
            &operation(
                Kind::VmConnector,
                "node-b",
                ActionType::Insert,
                2_000,
                &connector_fields("conn-1", "node-a"),
            ),
        )
        .expect_err("nor somebody else's connector");
        assert!(
            connector
                .to_string()
                .contains("may not create a core.vm_connector owned by 'node-a'"),
            "the owner rule must be the one that spoke: {connector}"
        );
        assert_eq!(scalar(&db, "SELECT id FROM vm_connectors"), None);
    }

    /// A `node` host IS a mesh node, so its `node_id` is the author's own id.
    /// Otherwise one node could publish the host row of another and own the
    /// machine list that hangs off it.
    #[test]
    fn a_node_host_may_only_be_published_by_the_node_it_names() {
        let db = make_db();
        let mut fields = host_fields("node-a", "node-b");
        // Owner and author agree; only the node the host claims to be differs.
        fields.retain(|(key, _)| *key != "node_id");
        fields.push(("node_id", text("node-a")));
        let refused = apply(
            &db,
            &operation(Kind::VmHost, "node-b", ActionType::Insert, 1_000, &fields),
        )
        .expect_err("a node may not publish another node's host row");
        assert!(refused.to_string().contains("host row of node 'node-a'"));
    }

    /// The id of a `node` host IS the node id (plan §4.1). A row that keyed
    /// itself differently would occupy a resource id nothing resolves to, and
    /// the partial unique index would then refuse the real row — as raw
    /// constraint text, on a second node, later.
    #[test]
    fn a_node_host_is_keyed_by_the_node_id() {
        let db = make_db();
        let mut fields = host_fields("host-not-a-node-id", "node-a");
        fields.retain(|(key, _)| *key != "id");
        fields.push(("id", text("host-not-a-node-id")));
        let refused = apply(
            &db,
            &operation(Kind::VmHost, "node-a", ActionType::Insert, 1_000, &fields),
        )
        .expect_err("a node host keyed by anything else is refused");
        assert!(
            refused.to_string().contains("must be keyed by that node id"),
            "message: {refused}"
        );
    }

    /// A row that has never existed is at epoch zero. Letting an insert start
    /// higher would hand a new row an epoch nobody can outbid.
    #[test]
    fn a_first_insert_may_not_start_at_a_nonzero_epoch() {
        let db = make_db();
        let mut fields = host_fields("node-a", "node-a");
        fields.retain(|(key, _)| *key != "owner_epoch");
        fields.push(("owner_epoch", FieldValue::I64(7)));
        let refused = apply(
            &db,
            &operation(Kind::VmHost, "node-a", ActionType::Insert, 1_000, &fields),
        )
        .expect_err("a new row starts at epoch zero");
        assert!(refused.to_string().contains("owner epoch 0"));
    }

    /// Plan §6.1: an Insert on an existing key is an Update, and the owner of
    /// the row that is already there wins. Without this an upsert would be a
    /// free takeover of any row whose id you can guess.
    #[test]
    fn an_upsert_from_a_stranger_cannot_take_over_an_existing_row() {
        let db = make_db();
        seed_host(&db, "node-a", 1_000);
        let mut fields = host_fields("node-a", "node-b");
        fields.retain(|(key, _)| *key != "node_id");
        fields.push(("node_id", text("node-a")));
        let refused = apply(
            &db,
            &operation(Kind::VmHost, "node-b", ActionType::Insert, 2_000, &fields),
        )
        .expect_err("an upsert may not take a row over");
        assert!(refused.to_string().contains("owned by 'node-a'"));
        assert_eq!(
            scalar(&db, "SELECT owner_node_id FROM vm_hosts WHERE id = 'node-a'"),
            Some("node-a".to_string())
        );
    }

    /// The owner writing its own row is the ordinary case, and it has to work.
    #[test]
    fn an_update_from_the_owner_lands() {
        let db = make_db();
        seed_host(&db, "node-a", 1_000);
        apply(
            &db,
            &operation(
                Kind::VmHost,
                "node-a",
                ActionType::Update,
                2_000,
                &[("id", text("node-a")), ("status", text("needs_install"))],
            ),
        )
        .expect("the owner may write its own row");
        assert_eq!(
            scalar(&db, "SELECT status FROM vm_hosts WHERE id = 'node-a'"),
            Some("needs_install".to_string())
        );
    }

    /// Plan §6.1: "stara epoka = ignorowana". An operation minted before a
    /// transfer is about a past the row has left — dropped, not refused, because
    /// refusing it would fill the inbox with conflicts about settled history.
    #[test]
    fn a_stale_epoch_is_ignored_not_refused() {
        let db = make_db();
        seed_host(&db, "node-a", 1_000);
        {
            let conn = db.write().expect("lock");
            conn.execute(
                "UPDATE vm_hosts SET owner_epoch = 3, owner_node_id = 'node-b' WHERE id = 'node-a'",
                [],
            )
            .expect("a transfer already happened");
        }
        let rows = apply(
            &db,
            &operation(
                Kind::VmHost,
                "node-a",
                ActionType::Update,
                2_000,
                &[
                    ("id", text("node-a")),
                    ("owner_epoch", FieldValue::I64(1)),
                    ("status", text("stale")),
                ],
            ),
        )
        .expect("a stale epoch is ignored, not an error");
        assert_eq!(rows, 0);
        assert_ne!(
            scalar(&db, "SELECT status FROM vm_hosts WHERE id = 'node-a'"),
            Some("stale".to_string())
        );
    }

    /// The transfer arm plan §6.1 describes for host migration: the SOURCE
    /// writes the switch after the target confirms, and the epoch is what makes
    /// it a transfer rather than a takeover.
    #[test]
    fn ownership_moves_with_the_next_epoch_and_only_from_the_owner() {
        let db = make_db();
        seed_host(&db, "node-a", 1_000);
        let handover = |actor: &str, wall: i64| {
            operation(
                Kind::VmHost,
                actor,
                ActionType::Update,
                wall,
                &[
                    ("id", text("node-a")),
                    ("owner_node_id", text("node-b")),
                    ("owner_epoch", FieldValue::I64(1)),
                ],
            )
        };
        let refused = apply(&db, &handover("node-b", 2_000))
            .expect_err("the receiving node may not bump the epoch by itself");
        assert!(refused.to_string().contains("owned by 'node-a'"));

        apply(&db, &handover("node-a", 3_000)).expect("the owner hands the row over");
        assert_eq!(
            scalar(&db, "SELECT owner_node_id FROM vm_hosts WHERE id = 'node-a'"),
            Some("node-b".to_string())
        );
        // And the new owner is now the one who may write it.
        apply(
            &db,
            &operation(
                Kind::VmHost,
                "node-b",
                ActionType::Update,
                4_000,
                &[("id", text("node-a")), ("status", text("ready"))],
            ),
        )
        .expect("the new owner writes the row");
    }

    /// Ownership moves with the epoch or not at all: an owner that could hand a
    /// row over silently would leave the receiver unable to tell that from the
    /// transfer the plan describes.
    #[test]
    fn ownership_may_not_move_without_bumping_the_epoch() {
        let db = make_db();
        seed_host(&db, "node-a", 1_000);
        let refused = apply(
            &db,
            &operation(
                Kind::VmHost,
                "node-a",
                ActionType::Update,
                2_000,
                &[("id", text("node-a")), ("owner_node_id", text("node-b"))],
            ),
        )
        .expect_err("ownership moves with the epoch");
        assert!(refused.to_string().contains("without bumping the epoch"));
    }

    /// Plan §6.1: "no parent = rejection", and deferrable, because the insert
    /// that creates the host may be behind this one in the inbox.
    #[test]
    fn a_machine_without_its_host_defers_instead_of_conflicting() {
        let db = make_db();
        let deferred = apply(
            &db,
            &operation(
                Kind::VmGuest,
                "node-a",
                ActionType::Insert,
                1_000,
                &guest_fields("guest-1", "node-a"),
            ),
        )
        .expect_err("a machine cannot land before its host");
        assert!(
            matches!(deferred, SyncLedgerError::DeferredOrdering(_)),
            "the host may still be behind it: {deferred:?}"
        );
    }

    /// A satellite has no owner column; it answers to the owner of the machine,
    /// and the machine to the owner of the host. Two hops, one rule.
    #[test]
    fn a_disk_answers_to_the_owner_of_its_host() {
        let db = make_db();
        seed_host(&db, "node-a", 1_000);
        apply(
            &db,
            &operation(
                Kind::VmGuest,
                "node-a",
                ActionType::Insert,
                2_000,
                &guest_fields("guest-1", "node-a"),
            ),
        )
        .expect("the host owner creates the machine");

        let disk = |actor: &str, wall: i64| {
            operation(
                Kind::VmGuestDisk,
                actor,
                ActionType::Insert,
                wall,
                &[
                    ("id", text("disk-1")),
                    ("guest_id", text("guest-1")),
                    ("instance_id", text("env-1")),
                    ("org_id", text(DEFAULT_ORG_ID)),
                    ("pool_id", text("pool-1")),
                    ("volume_ref", text("vol-1")),
                    ("bus", text("virtio")),
                    ("format", text("qcow2")),
                    ("size_bytes", FieldValue::I64(42_949_672_960)),
                    ("created_at", text("2026-09-05T00:00:00Z")),
                    ("updated_at", text("2026-09-05T00:00:00Z")),
                    ("updated_by_node", text(actor)),
                ],
            )
        };
        let refused = apply(&db, &disk("node-b", 3_000))
            .expect_err("a stranger may not add a disk to somebody else's machine");
        assert!(refused.to_string().contains("owned by 'node-a'"));
        apply(&db, &disk("node-a", 4_000)).expect("the host owner adds the disk");
        assert_eq!(
            scalar(&db, "SELECT volume_ref FROM vm_guest_disks WHERE id = 'disk-1'"),
            Some("vol-1".to_string())
        );
    }

    /// Both ends of a move have to answer to the author: the machine the disk
    /// hangs off now, and the machine it is being moved to. Otherwise the owner
    /// of an empty machine could pull a foreign disk onto it.
    #[test]
    fn a_disk_may_not_be_moved_onto_another_owners_machine() {
        let db = make_db();
        seed_host(&db, "node-a", 1_000);
        seed_host(&db, "node-b", 1_100);
        apply(
            &db,
            &operation(
                Kind::VmGuest,
                "node-a",
                ActionType::Insert,
                2_000,
                &guest_fields("guest-a", "node-a"),
            ),
        )
        .expect("machine on node-a");
        apply(
            &db,
            &operation(
                Kind::VmGuest,
                "node-b",
                ActionType::Insert,
                2_100,
                &guest_fields("guest-b", "node-b"),
            ),
        )
        .expect("machine on node-b");
        apply(
            &db,
            &operation(
                Kind::VmGuestDisk,
                "node-a",
                ActionType::Insert,
                3_000,
                &[
                    ("id", text("disk-1")),
                    ("guest_id", text("guest-a")),
                    ("instance_id", text("env-1")),
                    ("org_id", text(DEFAULT_ORG_ID)),
                    ("pool_id", text("pool-1")),
                    ("volume_ref", text("vol-1")),
                    ("bus", text("virtio")),
                    ("format", text("qcow2")),
                    ("size_bytes", FieldValue::I64(1)),
                    ("created_at", text("2026-09-05T00:00:00Z")),
                    ("updated_at", text("2026-09-05T00:00:00Z")),
                    ("updated_by_node", text("node-a")),
                ],
            ),
        )
        .expect("the disk belongs to node-a");

        let pull = apply(
            &db,
            &operation(
                Kind::VmGuestDisk,
                "node-b",
                ActionType::Update,
                4_000,
                &[("id", text("disk-1")), ("guest_id", text("guest-b"))],
            ),
        )
        .expect_err("node-b may not pull node-a's disk onto its own machine");
        assert!(pull.to_string().contains("owned by 'node-a'"), "message: {pull}");

        // The other direction, and it is the one only the STATED side catches:
        // node-a owns the disk it is moving, so the current parent answers to
        // the author — and the machine it is pushed onto does not. A mutation
        // that checked only the current parent left the case above green.
        let push = apply(
            &db,
            &operation(
                Kind::VmGuestDisk,
                "node-a",
                ActionType::Update,
                5_000,
                &[("id", text("disk-1")), ("guest_id", text("guest-b"))],
            ),
        )
        .expect_err("node-a may not push its disk onto node-b's machine");
        assert!(push.to_string().contains("owned by 'node-b'"), "message: {push}");
        assert_eq!(
            scalar(&db, "SELECT guest_id FROM vm_guest_disks WHERE id = 'disk-1'"),
            Some("guest-a".to_string())
        );
    }

    /// Environment policy has no node owner. Phase 0 checks the one thing the
    /// mesh can check — that the author acts for the organization — and defers
    /// otherwise, because the promotion may be queued behind this write.
    #[test]
    fn environment_policy_rows_come_from_operator_nodes_only() {
        let db = make_db();
        let tag = |actor: &str, wall: i64| {
            operation(
                Kind::VmTag,
                actor,
                ActionType::Insert,
                wall,
                &[
                    ("instance_id", text("env-1")),
                    ("name", text("prod")),
                    ("org_id", text(DEFAULT_ORG_ID)),
                    ("color", text("#ff0000")),
                    ("created_at", text("2026-09-05T00:00:00Z")),
                    ("updated_at", text("2026-09-05T00:00:00Z")),
                    ("updated_by_node", text(actor)),
                ],
            )
        };
        let deferred = apply(&db, &tag("node-plain", 1_000))
            .expect_err("a node that does not act for the organization may not write policy");
        assert!(matches!(deferred, SyncLedgerError::DeferredOrdering(_)));
        make_operator(&db, "node-op");
        apply(&db, &tag("node-op", 2_000)).expect("an operator writes environment policy");
        assert_eq!(
            scalar(&db, "SELECT color FROM vm_tags WHERE name = 'prod'"),
            Some("#ff0000".to_string())
        );
    }

    /// A pool with no host is the plan's "shared by several hosts", not an
    /// orphan: it has no owner node, so it falls under the organization rule.
    #[test]
    fn a_shared_pool_without_a_host_is_governed_by_the_organization() {
        let db = make_db();
        let pool = |actor: &str, wall: i64| {
            operation(
                Kind::VmStoragePool,
                actor,
                ActionType::Insert,
                wall,
                &[
                    ("id", text("pool-shared")),
                    ("org_id", text(DEFAULT_ORG_ID)),
                    ("host_id", FieldValue::Null),
                    ("kind", text("nfs")),
                    ("display_name", text("Shared")),
                    ("status", text("ready")),
                    ("created_at", text("2026-09-05T00:00:00Z")),
                    ("updated_at", text("2026-09-05T00:00:00Z")),
                    ("updated_by_node", text(actor)),
                ],
            )
        };
        let deferred = apply(&db, &pool("node-plain", 1_000))
            .expect_err("a shared pool is not writable by just anybody");
        assert!(matches!(deferred, SyncLedgerError::DeferredOrdering(_)));
        make_operator(&db, "node-op");
        apply(&db, &pool("node-op", 2_000)).expect("an operator declares the shared pool");
    }

    /// The twin of the rule above, on a row that already EXISTS — and until
    /// this test it was unguarded.
    ///
    /// `authorize` reaches the `None if nullable` arm twice: once through
    /// `authorize_first_insert`, which the test above covers, and once through
    /// the existing-row path, which nothing exercised. The measurement that
    /// named it (U3 of `07-rejestr-krytyk-r2.md`) was a mutation: deleting the
    /// `?` from `authorize_organization(...)?` in that second arm left 47 of 47
    /// green. A shared pool or network — the plan's "pula wspólna", the only
    /// rows in this registry with no parent and no owner — could therefore be
    /// REWRITTEN or DELETED by any trusted peer, however the fleet had voted on
    /// its operator list.
    ///
    /// It is closed here rather than in step 7 because the deletion half only
    /// becomes reachable with code that removes machines and their satellites,
    /// which is what this step's authorization work is the first half of.
    #[test]
    fn an_existing_shared_pool_is_still_governed_by_the_organization() {
        let db = make_db();
        let pool = |actor: &str, action: ActionType, name: &str, wall: i64| {
            operation(
                Kind::VmStoragePool,
                actor,
                action,
                wall,
                &[
                    ("id", text("pool-shared")),
                    ("org_id", text(DEFAULT_ORG_ID)),
                    ("host_id", FieldValue::Null),
                    ("kind", text("nfs")),
                    ("display_name", text(name)),
                    ("status", text("ready")),
                    ("created_at", text("2026-09-05T00:00:00Z")),
                    ("updated_at", text("2026-09-05T00:00:00Z")),
                    ("updated_by_node", text(actor)),
                ],
            )
        };
        make_operator(&db, "node-op");
        apply(&db, &pool("node-op", ActionType::Insert, "Shared", 1_000))
            .expect("an operator declares the shared pool");

        // From here the row EXISTS, so every operation below takes the second
        // arm — the one the mutation showed was decorative.
        for (action, what) in [
            (ActionType::Update, "rewrite"),
            (ActionType::Insert, "upsert over"),
            (ActionType::Delete, "delete"),
        ] {
            let error = apply(&db, &pool("node-plain", action, "Taken", 2_000))
                .expect_err("a node off the operator list may not touch a shared pool");
            // Deferrable, not terminal, for the same reason as everywhere else
            // in this file: the operation that puts the author on the operator
            // list may still be queued behind this one.
            assert!(
                matches!(error, SyncLedgerError::DeferredOrdering(_)),
                "{what}: expected a deferral, got {error:?}"
            );
        }

        // And the row is untouched — a refusal that still wrote would be worse
        // than no rule at all.
        assert_eq!(
            scalar(&db, "SELECT display_name FROM vm_storage_pools WHERE id = 'pool-shared'"),
            Some("Shared".to_string())
        );
    }

    /// The reseed problem, in one test. After a baseline reset every node
    /// restates every row it holds, most of them owned by somebody else. Those
    /// restatements ask for nothing, so they must be IGNORED rather than refused
    /// — a refusal becomes a deferral, a deferral becomes a terminal conflict,
    /// and one reset would poison every inbox with rows nobody was changing.
    #[test]
    fn a_restatement_of_an_agreed_row_is_ignored_not_refused() {
        let db = make_db();
        seed_host(&db, "node-a", 1_000);
        let rows = apply(
            &db,
            &operation(
                Kind::VmHost,
                "node-c",
                ActionType::Insert,
                2_000,
                &host_fields("node-a", "node-a"),
            ),
        )
        .expect("a restatement exercises no authority, so it needs none");
        assert_eq!(rows, 0);
    }

    /// An operation's identity must be the identity its content builds. A row
    /// that could declare a made-up `resource_id` would be ordered under a key
    /// nothing else uses, and LWW would compare it against nothing.
    #[test]
    fn a_resource_id_the_key_columns_do_not_build_is_refused() {
        let db = make_db();
        let mut op = operation(
            Kind::VmHost,
            "node-a",
            ActionType::Insert,
            1_000,
            &host_fields("node-a", "node-a"),
        );
        op.body.resource_id = "somebody-elses-id".to_string();
        let refused = apply(&db, &op).expect_err("a forged resource id is refused");
        assert!(refused.to_string().contains("key columns do not build"));
    }

    /// A value the DDL forbids is refused by name. Letting it reach SQLite would
    /// turn it into a terminal conflict carrying raw constraint text — unreadable
    /// for the operator and a schema leak on the wire.
    #[test]
    fn an_out_of_set_value_never_reaches_the_sql_check() {
        let db = make_db();
        seed_host(&db, "node-a", 1_000);
        let mut fields = guest_fields("guest-1", "node-a");
        fields.retain(|(key, _)| *key != "kind");
        fields.push(("kind", text("toaster")));
        let refused = apply(
            &db,
            &operation(Kind::VmGuest, "node-a", ActionType::Insert, 2_000, &fields),
        )
        .expect_err("an unknown machine kind is refused");
        let message = refused.to_string();
        assert!(message.contains("invalid kind: 'toaster'"), "message: {message}");
        assert!(
            !message.contains("CHECK constraint"),
            "the conflict reason must be ours, not SQLite's: {message}"
        );
    }

    /// The discriminator CHECK is what stops a connector host from claiming a
    /// mesh node id. It has to be evaluated on the row as it will stand, not on
    /// the fields the operation happens to carry.
    #[test]
    fn a_host_may_not_name_the_wrong_discriminator() {
        let db = make_db();
        let mut fields = host_fields("host-x", "node-a");
        fields.retain(|(key, _)| *key != "kind" && *key != "node_id");
        fields.push(("kind", text("connector_host")));
        fields.push(("node_id", text("node-a")));
        fields.push(("connector_id", text("conn-1")));
        let refused = apply(
            &db,
            &operation(Kind::VmHost, "node-a", ActionType::Insert, 1_000, &fields),
        )
        .expect_err("a connector host may not also be a node");
        let message = refused.to_string();
        assert!(message.contains("wrong discriminator"), "message: {message}");
        assert!(!message.contains("CHECK constraint"), "message: {message}");
    }

    /// A truncated operation is named, not left to SQLite: "does not state
    /// required columns: status" is a sentence an operator can act on, a raw
    /// NOT NULL failure is not.
    #[test]
    fn a_new_row_that_omits_a_required_column_is_refused_by_name() {
        let db = make_db();
        let mut fields = host_fields("node-a", "node-a");
        fields.retain(|(key, _)| *key != "status");
        let refused = apply(
            &db,
            &operation(Kind::VmHost, "node-a", ActionType::Insert, 1_000, &fields),
        )
        .expect_err("a new row must state what the schema needs");
        assert!(refused.to_string().contains("required columns: status"));
    }

    /// The POSITIVE half of the same rule, and the half that had no guard at
    /// all until a critic measured its absence.
    ///
    /// An authorized operation that changes no rows — a restatement of what the
    /// row already holds, which every reseed produces by the hundred — MUST
    /// take its place in the order. Otherwise a slower node's older edit
    /// arrives afterwards and wins, which is the whole failure LWW exists to
    /// prevent (step 5 closed exactly this for `sync_nodes`).
    ///
    /// Why this test exists at all: the round that fixed the negative half
    /// rewrote two tests that asserted the old behaviour, and the rewrite
    /// removed the ONLY place in the tree measuring that TentaVM rows are
    /// ordered by LWW in the first place. Three separate mutations — "a no-op
    /// stops stamping", "nothing TentaVM ever stamps", and one the implementer
    /// had already caught once — all went green across 279 tests of `sync::`.
    /// The report claiming "a change of assertion, not a weakening" was
    /// measurably wrong in this one dimension.
    #[test]
    fn an_authorized_restatement_takes_its_place_in_the_order() {
        let db = make_db();
        seed_host(&db, "node-a", 1_000);

        // Authorized, and asking for exactly what is already there.
        let rows = apply(
            &db,
            &operation(
                Kind::VmHost,
                "node-a",
                ActionType::Update,
                5_000,
                &[("id", text("node-a"))],
            ),
        )
        .expect("the owner may restate its own row");
        assert_eq!(rows, 0, "a restatement writes nothing");

        // …and an edit older than the restatement now loses, which is only
        // observable if the restatement moved the slot.
        let rows = apply(
            &db,
            &operation(
                Kind::VmHost,
                "node-a",
                ActionType::Update,
                3_000,
                &[("id", text("node-a")), ("display_name", text("stale"))],
            ),
        )
        .expect("an older edit is dropped, not an error");
        assert_eq!(rows, 0, "an edit older than the slot must not land");
        assert_ne!(
            scalar(&db, "SELECT display_name FROM vm_hosts").as_deref(),
            Some("stale"),
            "the restatement did not advance the LWW slot, so these rows are \
             not ordered at all"
        );

        // An edit NEWER than the restatement but OLDER than the last one below
        // must also land — and this step is what separates "a restatement takes
        // its place" from "any accepted write takes its place". Without it the
        // test passes even when nothing but a restatement is ever stamped,
        // which a critic measured on the first version of this test.
        let rows = apply(
            &db,
            &operation(
                Kind::VmHost,
                "node-a",
                ActionType::Update,
                6_000,
                &[("id", text("node-a")), ("display_name", text("middle"))],
            ),
        )
        .expect("an edit newer than the restatement lands");
        assert_eq!(rows, 1);

        let rows = apply(
            &db,
            &operation(
                Kind::VmHost,
                "node-a",
                ActionType::Update,
                4_000,
                &[("id", text("node-a")), ("display_name", text("older"))],
            ),
        )
        .expect("an older edit is dropped, not an error");
        assert_eq!(rows, 0, "an ordinary accepted write must move the slot too");
        assert_eq!(
            scalar(&db, "SELECT display_name FROM vm_hosts").as_deref(),
            Some("middle"),
            "the write at 6000 did not advance the slot"
        );

        // A newer edit still lands, so the guard above is about ORDER and not
        // about refusing everything.
        let rows = apply(
            &db,
            &operation(
                Kind::VmHost,
                "node-a",
                ActionType::Update,
                9_000,
                &[("id", text("node-a")), ("display_name", text("fresh"))],
            ),
        )
        .expect("a newer edit lands");
        assert_eq!(rows, 1);
        assert_eq!(
            scalar(&db, "SELECT display_name FROM vm_hosts").as_deref(),
            Some("fresh")
        );
    }

    /// The other two ways an operation reaches `Ok` without being authorized,
    /// measured on a row that already exists and belongs to somebody else.
    /// Neither may move the row's place in the order.
    ///
    /// Both are shaped so that the ownership rules never even compare the
    /// author with the owner: the first asks for exactly what the row already
    /// says (so the refusal is swallowed as "asking for nothing"), the second
    /// carries a stale epoch (so §6.1's "stara epoka = ignorowana" fires
    /// first). A stranger needs no privilege to build either — for a host of
    /// kind `node` the resource id is the public node id.
    #[test]
    fn an_unauthorized_operation_does_not_pin_the_order() {
        for (label, fields) in [
            ("restates the key and nothing else", vec![("id", text("node-a"))]),
            (
                "carries a stale epoch",
                vec![("id", text("node-a")), ("owner_epoch", int(-1))],
            ),
        ] {
            let db = make_db();
            seed_host(&db, "node-a", 1_000);

            let rows = apply(
                &db,
                &operation(
                    Kind::VmHost,
                    "node-hostile",
                    ActionType::Update,
                    i64::MAX,
                    &fields,
                ),
            )
            .unwrap_or_else(|e| panic!("{label}: expected a quiet refusal, got {e}"));
            assert_eq!(rows, 0, "{label}: nothing may be written");

            // The owner's own later edit, at an ordinary clock, still lands.
            let rows = apply(
                &db,
                &operation(
                    Kind::VmHost,
                    "node-a",
                    ActionType::Update,
                    2_000,
                    &[("id", text("node-a")), ("display_name", text("renamed"))],
                ),
            )
            .unwrap_or_else(|e| panic!("{label}: the owner must still be able to write: {e}"));
            assert_eq!(rows, 1, "{label}: the owner's edit is dropped as stale");
            assert_eq!(
                scalar(&db, "SELECT display_name FROM vm_hosts").as_deref(),
                Some("renamed"),
                "{label}: a refused operation pinned the slot"
            );
        }
    }

    /// A delete of a row this node does not have writes nothing: there is
    /// nothing to remove, and the row's absence is not a fact a peer's word
    /// should create.
    ///
    /// And it does not take a place in the LWW order either. It used to: the
    /// materializer stamped every operation it accepted, so a stranger sending
    /// a delete of a row that does not exist yet — with a clock from the future
    /// — pinned the slot, and the real owner's later insert was then dropped as
    /// stale. The resource id of a host of kind `node` is the public node id,
    /// so knowing what to aim at took no privilege at all. An operation the
    /// ownership rules refused now reports that fact separately from its row
    /// count, and only an authorized one is stamped.
    ///
    /// This does NOT close the clock-skew hole of step 17: the row's true owner
    /// can still pin its own slot with a clock from the future. What it closes
    /// is a node with no title doing it to somebody else.
    #[test]
    fn a_delete_of_a_row_this_node_never_had_writes_nothing() {
        let db = make_db();
        let rows = apply(
            &db,
            &operation(
                Kind::VmHost,
                "node-hostile",
                ActionType::Delete,
                i64::MAX,
                &[("id", text("node-a"))],
            ),
        )
        .expect("a delete of nothing is not an error");
        assert_eq!(rows, 0);
        assert_eq!(scalar(&db, "SELECT id FROM vm_hosts"), None);
        // Measured, not assumed: the refused operation left the slot alone, so
        // the owner's own insert — at an ordinary clock, a million years behind
        // the hostile one — still lands.
        seed_host_at_any_clock(&db, "node-a", 1_000);
        assert_eq!(
            scalar(&db, "SELECT id FROM vm_hosts").as_deref(),
            Some("node-a"),
            "a refused operation must not pin the LWW slot of a row it has no title to"
        );
    }

    /// The leftovers of a machine that is already gone must still be removable;
    /// the row is an orphan either way, and refusing would keep it forever.
    #[test]
    fn an_orphaned_satellite_can_still_be_deleted() {
        let db = make_db();
        seed_host(&db, "node-a", 1_000);
        apply(
            &db,
            &operation(
                Kind::VmGuest,
                "node-a",
                ActionType::Insert,
                2_000,
                &guest_fields("guest-1", "node-a"),
            ),
        )
        .expect("machine");
        {
            let conn = db.write().expect("lock");
            conn.execute(
                "INSERT INTO vm_guest_nics (id, guest_id, instance_id, org_id, network_id, model, \
                 created_at, updated_at, updated_by_node) \
                 VALUES ('nic-1', 'guest-1', 'env-1', ?1, 'net-1', 'virtio', 't', 't', 'node-a')",
                rusqlite::params![DEFAULT_ORG_ID],
            )
            .expect("nic");
            conn.execute("DELETE FROM vm_guests WHERE id = 'guest-1'", [])
                .expect("machine removed");
        }
        // With no parent there is no owner to compare the author against, so
        // the one title that does not depend on a parent is what stands: the
        // organization's operator list. Without it the rule read the actor not
        // once, and any trusted peer could sweep other nodes' orphans.
        //
        // The price, named rather than hidden by this fixture's
        // `make_operator`: a node that owns an orphan but holds no operator
        // title cannot sweep it either, and gets `DeferredOrdering` — which is
        // the right shape (the operation waits for the author to be listed
        // rather than failing terminally), but it IS a narrowing of the branch
        // whose whole purpose was not to block cleanup. Nothing deletes
        // machines yet, so nothing produces orphans yet; when step 6 or a
        // driver does, this is the first rule to re-measure.
        let refused = apply(
            &db,
            &operation(
                Kind::VmGuestNic,
                "node-stranger",
                ActionType::Delete,
                4_000,
                &[("id", text("nic-1"))],
            ),
        )
        .expect_err("a stranger has no title to somebody else's orphan");
        assert!(refused.to_string().contains("operator list"));
        assert_eq!(scalar(&db, "SELECT id FROM vm_guest_nics").as_deref(), Some("nic-1"));

        make_operator(&db, "node-a");
        let rows = apply(
            &db,
            &operation(
                Kind::VmGuestNic,
                "node-a",
                ActionType::Delete,
                5_000,
                &[("id", text("nic-1"))],
            ),
        )
        .expect("an orphan is removable");
        assert_eq!(rows, 1);
    }

    // =========================================================================
    // Step 6 — access-request convergence (W1, S2, S3, S8, S11)
    // =========================================================================

    const AR_INSTANCE: &str = "vm-inst";
    const AR_USER: &str = "u-ala";
    const AR_HOST: &str = "host-a";
    const AR_EXPIRES: &str = "2099-01-01T00:00:00Z";

    /// The request row as an operation from the requester's node.
    fn request_op(actor: &str, seq: &str, wall: i64) -> SyncOperation {
        let id = crate::tentavm::access::request_id(
            AR_INSTANCE,
            DEFAULT_ORG_ID,
            AR_USER,
            "host_role",
            Some(AR_HOST),
            Some("deploy"),
            seq,
        );
        let mut op = operation(
            Kind::VmAccessRequest,
            actor,
            ActionType::Insert,
            wall,
            &[
                ("id", text(&id)),
                ("instance_id", text(AR_INSTANCE)),
                ("org_id", text(DEFAULT_ORG_ID)),
                ("scope", text("host_role")),
                ("host_id", text(AR_HOST)),
                ("role", text("deploy")),
                ("reason", text("na test migracji")),
                ("requested_by", text(AR_USER)),
                ("requested_at", text("2026-09-06T10:00:00Z")),
                ("requested_seq", text(seq)),
                ("expires_at", text(AR_EXPIRES)),
                ("owner_node_id", text(actor)),
                ("created_at", text("2026-09-06T10:00:00Z")),
                ("updated_at", text("2026-09-06T10:00:00Z")),
                ("updated_by_node", text(actor)),
            ],
        );
        op.body.resource_id = id;
        op
    }

    fn request_id_of(seq: &str) -> String {
        crate::tentavm::access::request_id(
            AR_INSTANCE,
            DEFAULT_ORG_ID,
            AR_USER,
            "host_role",
            Some(AR_HOST),
            Some("deploy"),
            seq,
        )
    }

    /// One administrator's decision, from their node.
    fn decision_op(
        actor: &str,
        request: &str,
        who: &str,
        verdict: &str,
        seq: &str,
        decided_at: &str,
        wall: i64,
    ) -> SyncOperation {
        let id = crate::tentavm::access::decision_id(request, who, seq);
        let mut op = operation(
            Kind::VmAccessDecision,
            actor,
            ActionType::Insert,
            wall,
            &[
                ("id", text(&id)),
                ("request_id", text(request)),
                ("instance_id", text(AR_INSTANCE)),
                ("org_id", text(DEFAULT_ORG_ID)),
                ("decision", text(verdict)),
                ("note", text("")),
                ("decided_by", text(who)),
                ("decided_at", text(decided_at)),
                ("decided_seq", text(seq)),
                ("owner_node_id", text(actor)),
                ("created_at", text(decided_at)),
                ("updated_at", text(decided_at)),
                ("updated_by_node", text(actor)),
            ],
        );
        op.body.resource_id = id;
        op
    }

    /// The grant set as a comparable value.
    fn grants_of(db: &DbPool) -> Vec<(String, String, String, Option<String>)> {
        let conn = db.read().expect("lock");
        let mut stmt = conn
            .prepare(
                "SELECT subject_id, role, source, request_id FROM vm_host_grants \
                 ORDER BY subject_id",
            )
            .expect("prepare");
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("query")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("rows")
    }

    /// W1 / S3, and the reason the whole path is shaped the way it is.
    ///
    /// Two administrators decide the SAME request at once and disagree. The two
    /// operations are delivered to two nodes in OPPOSITE orders, through
    /// `apply_core_operation` — the real materializer, not a helper — and both
    /// nodes must end with the same state and the same `vm_host_grants`.
    ///
    /// This is what a mutable `state` column under LWW cannot do: one node would
    /// keep `approved` with a grant written and the other `rejected` with none,
    /// permanently, because a terminal state does not regress and nothing
    /// reconciles the two. Here the state is a fold over an append-only set, so
    /// order cannot matter — and this test is the measurement of that claim
    /// rather than the claim itself.
    #[test]
    fn two_administrators_deciding_at_once_converge_in_either_order() {
        let mut ends = Vec::new();
        for (first, second) in [("reject", "approve"), ("approve", "reject")] {
            let db = make_db();
            make_operator(&db, "node-req");
            let seq = "1000.0.node-req";
            let request = request_id_of(seq);
            apply(&db, &request_op("node-req", seq, 1_000)).expect("the request lands");

            // Ala on node-a and Bogdan on node-b, each owning their own row.
            let ops = [
                (
                    "node-a",
                    decision_op(
                        "node-a",
                        &request,
                        "u-ala",
                        first,
                        "2000.0.node-a",
                        "2026-09-06T11:00:00Z",
                        2_000,
                    ),
                ),
                (
                    "node-b",
                    decision_op(
                        "node-b",
                        &request,
                        "u-bogdan",
                        second,
                        "2001.0.node-b",
                        "2026-09-06T11:00:01Z",
                        2_001,
                    ),
                ),
            ];
            for (_, op) in &ops {
                apply(&db, op).expect("a decision from its own node lands");
            }

            let conn = db.read().expect("lock");
            let (stored, decisions) =
                crate::tentavm::access::load(&conn, AR_INSTANCE, DEFAULT_ORG_ID, &request)
                    .expect("load")
                    .expect("the request");
            let folded = crate::tentavm::access::fold(&stored, &decisions, "2026-09-06T12:00:00Z");
            drop(conn);
            ends.push((folded.state, grants_of(&db)));
        }

        assert_eq!(
            ends[0], ends[1],
            "the two delivery orders must not produce two different worlds"
        );
        assert_eq!(ends[0].0, "rejected", "a rejection anywhere wins");
        assert!(
            ends[0].1.is_empty(),
            "and the grant the approval would have written is not there: {:?}",
            ends[0].1
        );
    }

    /// The other half of W1: the approval alone DOES write the grant, and it
    /// writes it as a computed row. Without this the test above would pass on a
    /// projection that never writes anything.
    #[test]
    fn an_approval_alone_projects_the_grant_on_every_node() {
        let db = make_db();
        make_operator(&db, "node-req");
        let seq = "1000.0.node-req";
        let request = request_id_of(seq);
        apply(&db, &request_op("node-req", seq, 1_000)).expect("request");
        apply(
            &db,
            &decision_op(
                "node-a",
                &request,
                "u-ala",
                "approve",
                "2000.0.node-a",
                "2026-09-06T11:00:00Z",
                2_000,
            ),
        )
        .expect("approval");

        assert_eq!(
            grants_of(&db),
            vec![(
                AR_USER.to_string(),
                "deploy".to_string(),
                "access_request".to_string(),
                Some(request.clone())
            )],
            "the approval projects a grant, tagged as computed and tied to its request"
        );

        // And a rejection arriving AFTERWARDS takes it away — the property that
        // only exists because the grant is a function of the set.
        apply(
            &db,
            &decision_op(
                "node-b",
                &request,
                "u-bogdan",
                "reject",
                "2001.0.node-b",
                "2026-09-06T11:00:01Z",
                2_001,
            ),
        )
        .expect("rejection");
        assert!(
            grants_of(&db).is_empty(),
            "a losing approval's grant must be undone by the same mechanism"
        );
    }

    /// S11 / W5: two nodes filing "attempt 1" at the same moment produce TWO
    /// rows, because the key is an HLC stamp and not a counter either of them
    /// computed from what it could see.
    #[test]
    fn two_nodes_filing_at_once_produce_two_requests() {
        let db = make_db();
        make_operator(&db, "node-a");
        make_operator(&db, "node-b");
        apply(&db, &request_op("node-a", "1000.0.node-a", 1_000)).expect("first");
        apply(&db, &request_op("node-b", "1000.0.node-b", 1_000)).expect("second");

        let count: i64 = {
            let conn = db.read().expect("lock");
            conn.query_row("SELECT COUNT(*) FROM vm_access_requests", [], |r| r.get(0))
                .expect("count")
        };
        assert_eq!(count, 2, "same instant, same user, same host — two rows");
    }

    /// W4.1 in its strongest form: the audit is not rewritable because there is
    /// nothing on these rows to rewrite. Both tables are append-only, and the
    /// OWNER is refused too — which is the case a §6.1 ownership rule alone
    /// would let through.
    #[test]
    fn an_access_request_row_is_append_only_even_for_its_owner() {
        let db = make_db();
        make_operator(&db, "node-req");
        let seq = "1000.0.node-req";
        apply(&db, &request_op("node-req", seq, 1_000)).expect("the request lands");

        let mut rewrite = request_op("node-req", seq, 3_000);
        rewrite.body.changed_fields.insert(
            "reason".to_string(),
            text("potrzebuję manage na wszystkim"),
        );
        let error = apply(&db, &rewrite).expect_err("its own author may not rewrite it");
        assert!(
            matches!(error, SyncLedgerError::Runtime(_)),
            "terminal, not deferrable: {error:?}"
        );

        let mut removal = request_op("node-req", seq, 4_000);
        removal.body.action = ActionType::Delete;
        let error = apply(&db, &removal).expect_err("nor delete it");
        assert!(matches!(error, SyncLedgerError::Runtime(_)), "{error:?}");

        assert_eq!(
            scalar(&db, "SELECT reason FROM vm_access_requests"),
            Some("na test migracji".to_string()),
            "the text an administrator read is the text that stays"
        );
    }

    /// W4.3 / S5: the key of these rows is a digest of their own content, and a
    /// row whose id its fields do not build is refused before it lands.
    #[test]
    fn a_request_whose_id_its_content_does_not_build_is_refused() {
        let db = make_db();
        make_operator(&db, "node-req");
        let mut forged = request_op("node-req", "1000.0.node-req", 1_000);
        // Same declared id, different content — the shape a peer would use to
        // file a request "from" somebody else under an id of its choosing.
        forged
            .body
            .changed_fields
            .insert("requested_by".to_string(), text("u-somebody-else"));
        let error = apply(&db, &forged).expect_err("the digest does not describe the row");
        assert!(matches!(error, SyncLedgerError::Runtime(_)), "{error:?}");
        let rows: i64 = {
            let conn = db.read().expect("lock");
            conn.query_row("SELECT COUNT(*) FROM vm_access_requests", [], |r| r.get(0))
                .expect("count")
        };
        assert_eq!(rows, 0, "and nothing landed on the way to the refusal");
    }

    /// W3 / S8, the half that lives on the LEDGER side. The handler's own gate
    /// protects the node somebody clicked on; this protects every other node
    /// from a decision minted on a request whose term had already passed.
    ///
    /// Judged by `decided_at` against `expires_at` — the moment the decision was
    /// MADE — so a node that was offline for a week still accepts a decision
    /// that was valid when it was taken.
    #[test]
    fn a_decision_taken_after_the_term_is_refused_by_every_node() {
        let db = make_db();
        make_operator(&db, "node-req");
        let seq = "1000.0.node-req";
        let request = request_id_of(seq);
        let mut short = request_op("node-req", seq, 1_000);
        short
            .body
            .changed_fields
            .insert("expires_at".to_string(), text("2026-09-01T00:00:00Z"));
        // The id is built from fields the term is not one of, so it still holds.
        apply(&db, &short).expect("a request with a short term is still a request");

        let error = apply(
            &db,
            &decision_op(
                "node-a",
                &request,
                "u-ala",
                "approve",
                "2000.0.node-a",
                "2026-09-06T11:00:00Z",
                2_000,
            ),
        )
        .expect_err("decided a week after the term ended");
        assert!(matches!(error, SyncLedgerError::Runtime(_)), "{error:?}");
        assert!(
            grants_of(&db).is_empty(),
            "and approving IS executing, so no grant was written"
        );
    }

    /// A decision whose request has not arrived yet DEFERS rather than failing:
    /// the request may simply be behind it in the inbox.
    #[test]
    fn a_decision_ahead_of_its_request_waits_instead_of_conflicting() {
        let db = make_db();
        make_operator(&db, "node-a");
        let request = request_id_of("1000.0.node-req");
        let error = apply(
            &db,
            &decision_op(
                "node-a",
                &request,
                "u-ala",
                "approve",
                "2000.0.node-a",
                "2026-09-06T11:00:00Z",
                2_000,
            ),
        )
        .expect_err("nothing to decide about yet");
        assert!(
            matches!(error, SyncLedgerError::DeferredOrdering(_)),
            "deferrable, because the request can still arrive: {error:?}"
        );
    }

}
