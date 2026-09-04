// =============================================================================
// File: bus/replication/router.rs — process-global ALPN_BUS accept demux
//       (plan-app-platform §1.6/§7 W5)
// =============================================================================
//
// `mesh/iroh_manager.rs` holds exactly ONE `Option<BusAcceptHandler>` slot
// (`IrohMeshManager::bus_accept_handler`) — a leftover from the M2 world
// where one process ever ran one TentaBus engine. `plan-app-platform`
// turns TentaBus into a `singleton = false` native app: N instances can be
// enabled on the same node, each with its OWN `ReplicationManager`
// (`manager::ReplicationManagerConfig::instance_id`'s doc — one manager per
// instance, never shared), but the mesh still only ever calls ONE handler
// per accepted `ALPN_BUS` connection. Whoever installs that one handler
// LAST used to win outright (`ReplicationManager::install_accept_handler`,
// deleted by this file) — every other instance's replication traffic would
// simply never reach its manager.
//
// This module is the fix: install the mesh's ONE handler on every
// `register` call (unconditionally — the handler is stateless, so
// re-installing on a second/third instance's enable, or a second mesh in
// the same process, is free and correct; W5 review finding D3), and demux
// every accepted bi-stream by the `instance_id` its first frame names,
// using a process-global registry of every instance's manager.
// `PartitionKey`'s own doc (`manager.rs`) explains why a manager's internal
// `registry` needs no instance component once this demux happens BEFORE
// any `PartitionKey` lookup — this file is where that "before" actually
// happens.

use std::sync::{Arc, OnceLock, Weak};

use dashmap::DashMap;

use crate::bus::instance::BusInstanceId;
use crate::bus::replication::frames::{self, ReplFrame, ReplHelloAck, ReplLeoReply, ReplReject};
use crate::bus::replication::leader::frame_kind_name;
use crate::bus::replication::manager::{BusRecv, BusSend, ReplicationManager};
use crate::mesh::iroh_manager::{BusAcceptHandler, IrohMeshManager};

/// Every instance currently running replication on this node, keyed by its
/// own `BusInstanceId` — the demux table `route_stream` reads.
static MANAGERS: OnceLock<DashMap<BusInstanceId, Arc<ReplicationManager>>> = OnceLock::new();

fn managers() -> &'static DashMap<BusInstanceId, Arc<ReplicationManager>> {
    MANAGERS.get_or_init(DashMap::new)
}

/// W5 review round 2 finding 3: `route_stream`'s two unknown-instance
/// `warn!` sites log `hello.instance_id`/`query.instance_id` BEFORE any
/// shape validation succeeds (`BusInstanceId::parse`'s result is discarded
/// with `.ok()` — only the lookup outcome is used), so the value is a raw
/// CBOR string bounded only by `frames::MAX_FRAME_BYTES` (16 MiB), and
/// `warn!` runs at essentially every deployment's default log level with no
/// rate limiting. A shape-valid `BusInstanceId` is always exactly 17 bytes
/// (`tentabus-` + 8 hex digits); this caps well above that so a legitimate
/// mistyped/rotated id is still fully readable in the log, while an
/// adversarial multi-megabyte string is not.
const LOGGED_INSTANCE_ID_CAP: usize = 64;

fn truncate_for_log(s: &str) -> std::borrow::Cow<'_, str> {
    if s.len() <= LOGGED_INSTANCE_ID_CAP {
        return std::borrow::Cow::Borrowed(s);
    }
    // The guard above is in BYTES, so the cut must be too, or a multibyte
    // string slips through at up to 4x the cap: `chars().take(CAP)` on a
    // 30-char / 120-byte id yields the whole string back. `char_indices`
    // gives the last boundary at or below the cap, so this never splits a
    // code point and the reported remainder is always the real one.
    let end = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= LOGGED_INSTANCE_ID_CAP)
        .last()
        .unwrap_or(0);
    let head = &s[..end];
    std::borrow::Cow::Owned(format!("{head}…(+{} more bytes)", s.len() - end))
}

/// `native_on_enable`'s (§7 W6) route to the mesh manager `replication::
/// init` needs but does not itself own: `main.rs` stashes a `Weak` handle
/// here right after the mesh pipeline starts (before this file's own
/// `register` — the first instance to enable might race mesh startup), and
/// every later `native_on_enable` call upgrades it instead of threading the
/// mesh handle through the native app hook signature (`NativeAppContext`
/// carries no mesh reference — widening it for this alone would leak a
/// mesh-specific concern into every other native app's hook contract). A
/// `Weak`, not an `Arc`: this module must never be the reason
/// `IrohMeshManager` outlives mesh shutdown.
static MESH_MANAGER: OnceLock<parking_lot::RwLock<Weak<IrohMeshManager>>> = OnceLock::new();

fn mesh_manager_cell() -> &'static parking_lot::RwLock<Weak<IrohMeshManager>> {
    MESH_MANAGER.get_or_init(|| parking_lot::RwLock::new(Weak::new()))
}

/// Called once from `main.rs`, right after the mesh pipeline hands back its
/// manager — before any TentaBus instance has necessarily enabled yet.
pub fn set_mesh_manager(mesh: &Arc<IrohMeshManager>) {
    *mesh_manager_cell().write() = Arc::downgrade(mesh);
}

/// `native_on_enable`'s (§7 W6) own doc: `None` means either the mesh was
/// never started (single-node / mesh-disabled config — today's RF=1
/// behavior) or it has since shut down; either way replication is skipped,
/// not an error.
pub fn mesh_manager() -> Option<Arc<IrohMeshManager>> {
    mesh_manager_cell().read().upgrade()
}

/// Registers `mgr` under `id` without touching the mesh — the pure registry
/// mutation half of `register`, split out so tests can populate `MANAGERS`
/// directly with fakes, no real `IrohMeshManager`/`iroh` connection
/// required. Idempotent: registering the same id again just replaces the
/// `Arc` (mirrors `bus::init_instance`'s own re-enable idempotency).
pub fn register_manager(id: BusInstanceId, mgr: Arc<ReplicationManager>) {
    managers().insert(id, mgr);
}

/// Test-only registry probe (review finding F9): whether `id` currently
/// has a manager registered. `bus::native`'s own enable/disable tests use
/// this to prove `native_on_enable`'s replication half actually reaches
/// `router::register` and `native_on_disable`'s reaches `unregister_if_
/// current` — not just that the ENGINE registry (`bus::instances()`)
/// changes, which the router never observes on its own. Not used by any
/// production code path; `route_stream`'s own `managers().get(...)` is the
/// real lookup this mirrors.
#[cfg(test)]
pub(crate) fn is_registered_for_test(id: &BusInstanceId) -> bool {
    managers().contains_key(id)
}

/// Brings this instance's replication traffic into the demux: inserts
/// `mgr` into the registry, then installs this module's
/// `handle_inbound_connection` as `mesh`'s `ALPN_BUS` accept handler.
///
/// W5 review finding D3: installs UNCONDITIONALLY on every call, not just
/// the first — `IrohMeshManager::set_bus_accept_handler` is a plain
/// `*write = Some(handler)` and the handler itself is stateless (it
/// captures nothing; every stream is routed by `route_stream` reading the
/// frame's OWN `instance_id`), so re-installing costs one lock swap and is
/// always correct, never just idempotent. A once-only guard here bought no
/// idempotency the plain write does not already have for free, while
/// silently binding this demux to the FIRST `IrohMeshManager` ever passed
/// in: a second mesh in the same process (a mesh restart on config change,
/// or any in-process multi-mesh test) would then never get a handler
/// installed at all, and every inbound `ALPN_BUS` connection on that mesh
/// closes with `b"bus-disabled"` (`iroh_manager.rs`'s own accept arm,
/// logged only at `debug!`) — replication silently deaf on that mesh, with
/// `managers()` still reporting the instance as registered.
pub async fn register(mesh: &IrohMeshManager, id: BusInstanceId, mgr: Arc<ReplicationManager>) {
    register_manager(id, mgr);
    let handler: BusAcceptHandler = Arc::new(move |remote_hex, connection| {
        tokio::spawn(handle_inbound_connection(remote_hex, connection));
    });
    mesh.set_bus_accept_handler(handler).await;
}

/// Removes `id` from the demux table (`replication::stop`, §7 W5/W6's
/// `native_on_disable`/`native_teardown`). Does NOT uninstall the mesh
/// handler — it stays installed for whichever OTHER instances remain (or
/// for the next one to enable); an inbound frame for `id` after this call
/// simply falls into `route_stream`'s unknown-instance arm, the same
/// `UnknownInstance`/zero-reply a never-registered instance gets.
pub fn unregister(id: &BusInstanceId) {
    managers().remove(id);
}

/// Removes `id` from the demux table ONLY IF the manager CURRENTLY
/// registered under it is `mgr` (`Arc::ptr_eq`) — `replication::stop`'s own
/// entry point (`init.rs::stop`). Closes plan-app-platform §7 W6's carried-
/// over finding #1: `register`/`register_manager` are a plain `insert`, so
/// an enable→disable→enable cycle that replaces the `Arc` under the SAME
/// key leaves a late `stop()` call for the OLD manager racing a live NEW
/// one. Unconditional-by-id removal (plain `unregister`, still used by
/// tests cleaning up their OWN ids) would then unregister the NEW, healthy
/// manager from the demux: the instance keeps running (`bus::instance`
/// still resolves it), but every inbound `Hello`/`LeoQuery` for it now
/// falls into `route_stream`'s unknown-instance arm — deaf, not dead, which
/// is worse to notice. A mismatch here is a silent no-op: whichever manager
/// the router actually still has is the correct one to keep, and this
/// call's caller (an old manager's own `stop`) has nothing further to do
/// either way.
pub fn unregister_if_current(id: &BusInstanceId, mgr: &Arc<ReplicationManager>) {
    managers().remove_if(id, |_, existing| Arc::ptr_eq(existing, mgr));
}

async fn handle_inbound_connection(remote_hex: String, connection: iroh::endpoint::Connection) {
    while let Ok((send, recv)) = connection.accept_bi().await {
        let remote = remote_hex.clone();
        tokio::spawn(route_stream(remote, Box::new(recv), Box::new(send)));
    }
}

/// Reads the first frame of one freshly accepted bi-stream and routes it to
/// the manager whose `BusInstanceId` the frame itself names:
///
/// - `Hello` for an unregistered/unparseable instance -> `HelloAck{
///   accepted: false, reject: UnknownInstance }`, stream then closed (the
///   caller drops `send`/`recv` on return, same as every other
///   `accept_hello` reject path), logged at `warn!` (D4 below);
/// - `LeoQuery` for an unregistered/unparseable instance -> `LeoReply{
///   leo: 0, hw: 0, leader_epoch: 0, in_isr: false }` — `ReplLeoReply` has
///   no `reject` field, so this mirrors the existing "let the candidate's
///   own deadline resolve it" behavior an unknown PARTITION already gets
///   (`ReplicationManager::answer_leo_query`'s doc), now also covering an
///   unknown INSTANCE, also logged at `warn!`. Safe in the direction that
///   matters — `election::choose_candidate` picks the HIGHEST leo, so an
///   answering peer that got zeroed here simply loses the vote, never wins
///   one it should not — but not risk-free in the mirror direction worth
///   naming rather than leaving silent: a peer that is genuinely AHEAD yet
///   only momentarily unregistered (mid `native_on_disable`, between
///   `router::unregister` and its manager's own shutdown finishing) also
///   answers zero, so a candidate could self-elect on less data than truly
///   exists and later `SendTruncate` that ahead replica when it reappears.
///   Split-brain safety does not depend on this reply being accurate (the
///   epoch/ISR admission gate is what actually prevents two leaders), so
///   this is a data-freshness footgun, not a safety one — worth a fix if
///   `native_on_disable`'s ordering ever needs revisiting, not urgent today;
/// - anything else (a frame kind that must never open a stream, or a
///   decode error) is dropped silently at the protocol level (matching
///   `ReplicationManager::accept_stream`'s own `_ => return` arm), logged
///   at `debug!` only — this arm is reachable by any TCP-level noise on the
///   ALPN, not just a misbehaving peer, so it does not warrant `warn!`.
///
/// W5 review finding D4: every arm below that used to be silent now logs
/// with the peer's `remote_hex` and (where known) the instance id involved
/// — this router is the ONLY mechanism enforcing "two instances must never
/// see each other's data", and it previously had no detection surface at
/// all: an operator could not tell a misconfigured peer dialing the wrong
/// instance id from someone probing which instances this node hosts.
///
/// W5 review finding D5: the `HelloAck.environment` on the `UnknownInstance`
/// arm is `hello.environment` — the DIALER's own claimed environment
/// echoed back, not this node's. Every other `HelloAck` on this ALPN
/// reports the RESPONDER's own environment (`manager.rs`'s `reject_ack`,
/// `follower.rs`'s Hello-accepted path) — this is the one exception,
/// because `route_stream` runs before any manager (and therefore any
/// `local_env`) has been resolved for an instance nobody here recognizes.
/// Reading this node's own environment fresh (`services::environment::
/// get_node_environment`) would need a `DbPool` threaded through this free
/// function and `register`/`handle_inbound_connection` — a real API
/// widening for a field nothing downstream reads today (`ReplHelloAck.
/// environment` has no consumer once `reject` is `Some`). Left undone,
/// documented here instead: `environment` on an `UnknownInstance` ack is
/// UNSPECIFIED — never this node's own claim, and callers must not read it
/// as one.
///
/// A free function, not a method on `ReplicationManager` — no single
/// manager owns "which instance is this" before this function has already
/// answered that question, and once it has, `accept_hello`/
/// `answer_leo_query` are the manager's own `pub(crate)` entry points
/// (public within this module chain is exactly why this file, not a mesh
/// change, is where §1.6's demux lives). `pub` (not `pub(crate)`) so
/// integration tests can drive it directly against `register_manager`-
/// populated fakes, with no real `iroh::endpoint::Connection` to accept
/// from.
pub async fn route_stream(remote_hex: String, mut recv: BusRecv, mut send: BusSend) {
    match frames::read_frame(&mut recv).await {
        Ok(ReplFrame::Hello(hello)) => {
            let target = BusInstanceId::parse(&hello.instance_id)
                .ok()
                .and_then(|id| managers().get(&id).map(|e| e.value().clone()));
            match target {
                Some(mgr) => mgr.accept_hello(hello, recv, send).await,
                None => {
                    tracing::warn!(
                        peer = %remote_hex,
                        instance_id = %truncate_for_log(&hello.instance_id),
                        "replication::router: Hello named an unregistered/unparseable \
                         instance — replying UnknownInstance"
                    );
                    let ack = ReplHelloAck {
                        accepted: false,
                        follower_leo: 0,
                        follower_hw: 0,
                        follower_epoch: 0,
                        // D5: the DIALER's own claimed environment, not
                        // this node's — see this function's own doc.
                        // UNSPECIFIED on this arm; do not read as ours.
                        environment: hello.environment,
                        reject: Some(ReplReject::UnknownInstance),
                    };
                    let _ = frames::write_frame(&mut send, &ReplFrame::HelloAck(ack)).await;
                }
            }
        }
        Ok(ReplFrame::LeoQuery(query)) => {
            let target = BusInstanceId::parse(&query.instance_id)
                .ok()
                .and_then(|id| managers().get(&id).map(|e| e.value().clone()));
            match target {
                Some(mgr) => mgr.answer_leo_query(query, send).await,
                None => {
                    tracing::warn!(
                        peer = %remote_hex,
                        instance_id = %truncate_for_log(&query.instance_id),
                        "replication::router: LeoQuery named an unregistered/unparseable \
                         instance — replying zeroed LeoReply"
                    );
                    let reply = ReplFrame::LeoReply(ReplLeoReply {
                        leo: 0,
                        hw: 0,
                        leader_epoch: 0,
                        in_isr: false,
                    });
                    let _ = frames::write_frame(&mut send, &reply).await;
                }
            }
        }
        Ok(other) => {
            // W5 review round 2 finding 2: log the frame KIND only, never
            // `?other` — `ReplFrame::Batch { bytes: Bytes }` can carry up to
            // `MAX_FRAME_BYTES` (16 MiB, `frames.rs`), and
            // `handle_inbound_connection` loops `accept_bi` with no
            // per-connection cap, so a trusted peer opening one stream per
            // 16 MiB `Batch` on a `debug!`-enabled node would otherwise burn
            // CPU escaping megabytes into the log on every single stream,
            // unbounded.
            tracing::debug!(
                peer = %remote_hex,
                frame_kind = frame_kind_name(&other),
                "replication::router: dropped a stream whose first frame is neither \
                 Hello nor LeoQuery"
            );
        }
        Err(e) => {
            tracing::debug!(
                peer = %remote_hex,
                error = %e,
                "replication::router: failed to decode the first frame of an inbound stream"
            );
        }
    }
}

/// Every `ReplicationManager` currently registered with the demux —
/// `tentaflow/src/main.rs` shutdown iterates this to stop each instance's
/// replication (`replication::init::stop`) before that instance's own
/// engine sweeper (plan-app-platform §1.8's shutdown row: "iterates
/// `bus::running_instances()` -> `replication::init::stop` + `svc.
/// shutdown()`" — this is the replication half, keyed independently since
/// not every running `BusService` necessarily has replication started,
/// e.g. mesh-disabled single-node builds).
pub fn running_managers() -> Vec<Arc<ReplicationManager>> {
    managers().iter().map(|e| e.value().clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::replication::assignment::PartitionAssignment;
    use crate::bus::replication::frames::{ReplHello, ReplLeoQuery};
    use crate::bus::replication::manager::{
        AssignmentStore, FollowerRunner, FollowerRunnerFactory, LeaderHandle, LeaderHandleFactory,
        LedgerAdmission, ReplAudit, ReplicationManagerConfig,
    };
    use crate::bus::ReplError;
    use crate::sync::ledger::OperationId;
    use tentaflow_protocol::environment::NodeEnvironment;
    use tokio::io::split;

    // ---- W5 review round 2 finding 6 fixture -------------------------------
    //
    // The three tests below prove "an unregistered/unparseable instance_id
    // is refused", but against an EMPTY `MANAGERS` table that proof was
    // vacuous: `managers().get(&any_id)` returns `None` for every key when
    // the map has zero entries, so a router that dropped the
    // `BusInstanceId::parse(...).ok()` guard entirely (or looked up a
    // hardcoded/wrong key by accident) would still make every assertion
    // below pass. `with_decoy_manager` registers one REAL (never-driven)
    // manager under a THIRD, distinct id before each test body runs, so a
    // `None` result for a DIFFERENT id now actually proves "this specific
    // id was not found in a non-empty registry" — the router's `.get` was
    // exercised against real data, not a trivially-empty map. Every trait
    // impl below `unimplemented!()`s: no test in this file ever drives an
    // inbound stream INTO the decoy manager, only ever looks it up by an id
    // that does not match it.
    struct NeverDrivenTransport;
    #[async_trait::async_trait]
    impl crate::bus::replication::manager::Transport for NeverDrivenTransport {
        async fn open_stream(&self, _node_id: &str) -> Result<(BusRecv, BusSend), ReplError> {
            unimplemented!("decoy fixture: never driven")
        }
    }
    struct NeverDrivenLedger;
    impl LedgerAdmission for NeverDrivenLedger {
        fn admitted_by(&self, _op_id: OperationId) -> Vec<String> {
            unimplemented!("decoy fixture: never driven")
        }
    }
    struct NeverDrivenAssignments;
    impl AssignmentStore for NeverDrivenAssignments {
        fn get(
            &self,
            _instance_id: &str,
            _org: &str,
            _topic: &str,
            _partition: u32,
        ) -> Result<Option<PartitionAssignment>, ReplError> {
            unimplemented!("decoy fixture: never driven")
        }
        fn list_for_topic(
            &self,
            _instance_id: &str,
            _org: &str,
            _topic: &str,
        ) -> Result<Vec<PartitionAssignment>, ReplError> {
            unimplemented!("decoy fixture: never driven")
        }
        fn list_for_node(
            &self,
            _instance_id: &str,
            _node_id: &str,
        ) -> Result<Vec<PartitionAssignment>, ReplError> {
            unimplemented!("decoy fixture: never driven")
        }
        fn propose(&self, _assignment: PartitionAssignment) -> Result<OperationId, ReplError> {
            unimplemented!("decoy fixture: never driven")
        }
    }
    struct NeverDrivenLeaderFactory;
    impl LeaderHandleFactory for NeverDrivenLeaderFactory {
        fn spawn(
            &self,
            _assignment: &PartitionAssignment,
            _replica_streams: Vec<(String, BusRecv, BusSend)>,
        ) -> Result<Box<dyn LeaderHandle>, ReplError> {
            unimplemented!("decoy fixture: never driven")
        }
    }
    struct NeverDrivenFollowerFactory;
    impl FollowerRunnerFactory for NeverDrivenFollowerFactory {
        fn spawn(
            &self,
            _assignment: &PartitionAssignment,
            _hello: ReplHello,
            _leader_recv: BusRecv,
            _leader_send: BusSend,
        ) -> Result<Box<dyn FollowerRunner>, ReplError> {
            unimplemented!("decoy fixture: never driven")
        }
    }
    struct NeverDrivenAudit;
    impl ReplAudit for NeverDrivenAudit {
        fn failover(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _from_node: Option<&str>,
            _to_node: &str,
            _from_epoch: u32,
            _to_epoch: u32,
            _duration_ms: u64,
            _reason: &str,
        ) {
            unimplemented!("decoy fixture: never driven")
        }
        fn transfer(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _from: &str,
            _to: &str,
            _epoch: u32,
        ) {
            unimplemented!("decoy fixture: never driven")
        }
        fn evicted(&self, _node_id: &str, _reason: &str, _count: u32) {
            unimplemented!("decoy fixture: never driven")
        }
    }

    /// Each caller passes its OWN decoy id. `MANAGERS` is process-global and
    /// cargo runs these tests on parallel threads, while `Guard::drop`
    /// removes by KEY, not by identity — so a single shared id lets one
    /// test's guard delete the entry another test is still relying on
    /// (`A.insert -> B.insert -> A.drop -> B.lookup` leaves the map empty
    /// exactly when B asserts). That never produces a false failure, only a
    /// false PROOF: the map goes back to being vacuously empty, which is the
    /// condition this fixture exists to rule out.
    fn with_decoy_manager(decoy_id: &str) -> impl Drop {
        struct Guard(BusInstanceId);
        impl Drop for Guard {
            fn drop(&mut self) {
                unregister(&self.0);
            }
        }
        let id = BusInstanceId::parse(decoy_id).expect("shape-valid decoy id");
        let manager = ReplicationManager::new(ReplicationManagerConfig {
            instance_id: decoy_id.to_string(),
            local_node_id: "decoy".to_string(),
            local_env: NodeEnvironment::Prod,
            transport: Arc::new(NeverDrivenTransport),
            ledger: Arc::new(NeverDrivenLedger),
            assignments: Arc::new(NeverDrivenAssignments),
            leader_factory: Arc::new(NeverDrivenLeaderFactory),
            follower_factory: Arc::new(NeverDrivenFollowerFactory),
            audit: Arc::new(NeverDrivenAudit),
            leo_query_timeout: std::time::Duration::from_millis(60),
            majority_await_timeout: std::time::Duration::from_millis(150),
        });
        register_manager(id.clone(), manager);
        Guard(id)
    }

    #[tokio::test]
    async fn route_stream_answers_unknown_instance_hello_with_reject() {
        let _decoy = with_decoy_manager("tentabus-decade01");
        let id = BusInstanceId::parse("tentabus-0badc0de").expect("shape-valid, never registered");
        // Defensive: make sure a stray earlier test never left this id
        // registered (process-global state, `MANAGERS` is shared across
        // every test in this binary).
        unregister(&id);

        let (client, server) = tokio::io::duplex(16 * 1024);
        let (mut client_recv, mut client_send) = split(client);
        let (server_recv, server_send) = split(server);
        tokio::spawn(route_stream(
            "peer".to_string(),
            Box::new(server_recv),
            Box::new(server_send),
        ));

        frames::write_frame(
            &mut client_send,
            &ReplFrame::Hello(ReplHello {
                instance_id: "tentabus-0badc0de".to_string(),
                org_id: "org".to_string(),
                topic: "orders".to_string(),
                partition: 0,
                leader_node_id: "l".to_string(),
                leader_epoch: 1,
                replicas: vec!["l".to_string()],
                environment: NodeEnvironment::Prod,
            }),
        )
        .await
        .expect("write Hello");

        match frames::read_frame(&mut client_recv)
            .await
            .expect("read HelloAck")
        {
            ReplFrame::HelloAck(ack) => {
                assert!(!ack.accepted);
                assert_eq!(ack.reject, Some(ReplReject::UnknownInstance));
            }
            other => panic!("expected HelloAck, got {other:?}"),
        }
    }

    // W5 review finding T2: covers the router's OWN half of "empty
    // instance_id is never some instance" — `BusInstanceId::parse("")`
    // fails shape validation, so `route_stream`'s
    // `.ok().and_then(|id| managers().get(&id)...)` must resolve to `None`
    // for an empty string exactly like it does for a shape-valid-but-
    // unregistered one (the sibling test above), proven by driving a real
    // empty-`instance_id` Hello through it rather than by inspection only.
    #[tokio::test]
    async fn route_stream_treats_an_empty_instance_id_hello_as_unknown() {
        let _decoy = with_decoy_manager("tentabus-decade02");
        let (client, server) = tokio::io::duplex(16 * 1024);
        let (mut client_recv, mut client_send) = split(client);
        let (server_recv, server_send) = split(server);
        tokio::spawn(route_stream(
            "peer".to_string(),
            Box::new(server_recv),
            Box::new(server_send),
        ));

        frames::write_frame(
            &mut client_send,
            &ReplFrame::Hello(ReplHello {
                instance_id: String::new(),
                org_id: "org".to_string(),
                topic: "orders".to_string(),
                partition: 0,
                leader_node_id: "l".to_string(),
                leader_epoch: 1,
                replicas: vec!["l".to_string()],
                environment: NodeEnvironment::Prod,
            }),
        )
        .await
        .expect("write Hello");

        match frames::read_frame(&mut client_recv)
            .await
            .expect("read HelloAck")
        {
            ReplFrame::HelloAck(ack) => {
                assert!(!ack.accepted);
                assert_eq!(ack.reject, Some(ReplReject::UnknownInstance));
            }
            other => panic!("expected HelloAck, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn route_stream_answers_unknown_instance_leo_query_with_zeroes() {
        let _decoy = with_decoy_manager("tentabus-decade03");
        let id = BusInstanceId::parse("tentabus-0badc0de").expect("shape-valid, never registered");
        unregister(&id);

        let (client, server) = tokio::io::duplex(16 * 1024);
        let (mut client_recv, mut client_send) = split(client);
        let (server_recv, server_send) = split(server);
        tokio::spawn(route_stream(
            "peer".to_string(),
            Box::new(server_recv),
            Box::new(server_send),
        ));

        frames::write_frame(
            &mut client_send,
            &ReplFrame::LeoQuery(ReplLeoQuery {
                instance_id: "tentabus-0badc0de".to_string(),
                org_id: "org".to_string(),
                topic: "orders".to_string(),
                partition: 0,
                known_epoch: 5,
            }),
        )
        .await
        .expect("write LeoQuery");

        match frames::read_frame(&mut client_recv)
            .await
            .expect("read LeoReply")
        {
            ReplFrame::LeoReply(r) => {
                assert_eq!((r.leo, r.hw, r.leader_epoch, r.in_isr), (0, 0, 0, false));
            }
            other => panic!("expected LeoReply, got {other:?}"),
        }
    }

    /// Builds a real (never-driven) `ReplicationManager` for `instance_id`,
    /// same fixture shape as `with_decoy_manager` but WITHOUT registering
    /// it — the caller decides when/whether to register, since this test
    /// exercises two DIFFERENT managers under the same id.
    fn build_manager(instance_id: &str) -> Arc<ReplicationManager> {
        ReplicationManager::new(ReplicationManagerConfig {
            instance_id: instance_id.to_string(),
            local_node_id: "decoy".to_string(),
            local_env: NodeEnvironment::Prod,
            transport: Arc::new(NeverDrivenTransport),
            ledger: Arc::new(NeverDrivenLedger),
            assignments: Arc::new(NeverDrivenAssignments),
            leader_factory: Arc::new(NeverDrivenLeaderFactory),
            follower_factory: Arc::new(NeverDrivenFollowerFactory),
            audit: Arc::new(NeverDrivenAudit),
            leo_query_timeout: std::time::Duration::from_millis(60),
            majority_await_timeout: std::time::Duration::from_millis(150),
        })
    }

    /// W6 carried-over finding #1: `unregister_if_current` must remove the
    /// OLD manager it names but leave a NEW manager registered under the
    /// SAME id untouched — the exact enable→disable→enable race
    /// `replication::init::stop`'s doc describes. A plain by-id
    /// `unregister` would fail this test (it cannot tell the two `Arc`s
    /// apart at all).
    #[test]
    fn unregister_if_current_leaves_a_replacement_manager_registered() {
        let id = BusInstanceId::parse("tentabus-1de17700").expect("shape-valid id");
        let old = build_manager(id.as_str());
        register_manager(id.clone(), old.clone());
        let new = build_manager(id.as_str());
        // Simulates a re-enable that replaced the `Arc` under the same key
        // (`register`/`register_manager` are a plain `insert`, never a
        // compare-and-swap) BEFORE the old manager's own `stop()` call runs.
        register_manager(id.clone(), new.clone());

        unregister_if_current(&id, &old);
        assert!(
            managers().get(&id).is_some(),
            "the NEW manager must still be registered — a stale stop() for \
             the OLD one must not deafen the live instance"
        );
        assert!(
            Arc::ptr_eq(&managers().get(&id).unwrap(), &new),
            "the surviving entry must be the NEW manager specifically"
        );

        unregister_if_current(&id, &new);
        assert!(
            managers().get(&id).is_none(),
            "a stop() for the CURRENTLY registered manager must remove it"
        );
    }

    /// Loopback, discovery-disabled `IrohMeshManager` — same pattern as
    /// `mesh::iroh_manager::tie_break_tests::make_manager` (that helper is
    /// private to its own module, so this is a self-contained copy rather
    /// than a cross-module dependency). Cheap: binds one ephemeral UDP
    /// socket, no network exchange, no accept loop started (this module
    /// never calls anything that would spawn one), so nothing but this
    /// function's own `Arc<IrohMeshManager>` ever holds a strong reference
    /// to it — required for the drop-releases-the-weak assertion below to
    /// be meaningful rather than accidentally true.
    async fn make_test_mesh_manager() -> Arc<IrohMeshManager> {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS trusted_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                public_key TEXT NOT NULL,
                hostname TEXT DEFAULT '',
                approved_by TEXT DEFAULT '',
                approved_at TEXT NOT NULL DEFAULT (datetime('now')),
                is_active INTEGER NOT NULL DEFAULT 1,
                last_addresses TEXT NOT NULL DEFAULT '',
                environment TEXT
            );
            CREATE TABLE IF NOT EXISTS pending_pairings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                remote_node_id TEXT NOT NULL,
                pin_code TEXT NOT NULL,
                direction TEXT NOT NULL CHECK(direction IN ('outgoing','incoming')),
                expires_at TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS revoked_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                revoked_by TEXT,
                revoked_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .expect("create tables");
        let db: crate::db::DbPool = Arc::new(crate::db::Db::from_connection(conn));
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));
        let security =
            Arc::new(crate::mesh::security::MeshSecurity::new(db, cipher).expect("security new"));
        let cfg = crate::mesh::iroh_manager::IrohMeshConfig {
            node_id: String::new(),
            bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_url: None,
            enable_lan_discovery: false,
            enable_dht_discovery: false,
            ..Default::default()
        };
        IrohMeshManager::new(cfg, security)
            .await
            .expect("manager new")
    }

    // W5 review finding T1: the test this replaces built a LOCAL
    // `parking_lot::RwLock<Weak<IrohMeshManager>>` and asserted
    // `std::sync::Weak`'s own upgrade contract — `set_mesh_manager`/
    // `mesh_manager` were never called, so a broken implementation of
    // EITHER could not have failed it. This drives both through a real
    // `Arc<IrohMeshManager>`: `set_mesh_manager` must make `mesh_manager()`
    // resolve to the SAME `Arc` (not merely `Some` of anything), and
    // dropping every strong reference must make it resolve to `None`
    // again — the `Weak`, never owning, contract `set_mesh_manager`'s own
    // doc promises.
    #[tokio::test]
    async fn set_mesh_manager_round_trips_through_mesh_manager_and_clears_on_drop() {
        let mgr = make_test_mesh_manager().await;
        set_mesh_manager(&mgr);

        let resolved =
            mesh_manager().expect("mesh_manager() must resolve right after set_mesh_manager");
        assert!(
            Arc::ptr_eq(&resolved, &mgr),
            "mesh_manager() must return the SAME Arc that was set, not merely some manager"
        );
        drop(resolved);
        drop(mgr);

        assert!(
            mesh_manager().is_none(),
            "mesh_manager() must be None once every strong Arc has been dropped — \
             this module must never be the reason IrohMeshManager outlives mesh shutdown"
        );
    }
}
