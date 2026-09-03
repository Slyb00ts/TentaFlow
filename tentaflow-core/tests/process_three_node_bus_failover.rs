// =============================================================================
// File: tests/process_three_node_bus_failover.rs
// Purpose: TentaBus M2 process-level chaos gate (SUM/tentabus/PLAN-M2.md
//          §1g / PLAN.md §5.2 P6-P9): 3 real OS processes, real iroh mesh +
//          pairing, real `BusService` + `ReplicationManager` over the real
//          ALPN_BUS wire protocol. `child.kill()` (SIGKILL) the leader mid
//          workload, measure P8 (last ACK before kill -> first ACK from the
//          promoted leader), assert zero loss of acked records at
//          `acks=quorum`, restart the killed node and byte-for-byte compare
//          logs up to min(hw) across all three nodes. A Z12 variant proves a
//          `node_environment = Test` node never joins the ISR of a Prod
//          topic and its accept is rejected on the real ALPN.
//
// Pattern: mirrors `tests/process_four_node_sync.rs` (parent spawns this same
// test binary as child processes, each with its own TENTAFLOW_HOME/db/
// ledger, real `IrohMeshManager` + `sync::runtime::init`, stdin-driven
// commands) with TentaBus-specific plumbing added on top:
//   - `bus::init` + `bus::replication::init` per child, mirroring
//     `tentaflow/src/main.rs`'s wiring order (mesh first, then bus, then
//     replication once the mesh manager and BusService both exist).
//   - A background "auto sync" task per child (push-if-pending, falling back
//     to a repair pull, every 150 ms, for every trusted peer, plus an inbox
//     drain every 150 ms) standing in for the real app's periodic outbox
//     drain (`mesh/pipeline.rs`'s on-connect push + retry loop) that this
//     lightweight harness does not otherwise run. Needed so leader election
//     itself (K-M2-3's LeoQuery + ledger-backed proposal +
//     `admitted_by_majority`, `election.rs`'s `MAJORITY_AWAIT_TIMEOUT` =
//     1.5 s) can complete autonomously after a `child.kill()` whose timing
//     the parent does not otherwise synchronize with.
//
// KNOWN GAP FOUND WHILE WRITING THIS TEST (documented in M2-WYNIKI.md):
// `BusService::create_topic`'s automatic replica-placement bootstrap
// (`bus/mod.rs`, the `if let Some(coordinator) = &coordinator { ... }`
// block right after `topics::create_topic`) can never fire on a genuinely
// fresh org/topic against the REAL `ReplicationManager`: it calls
// `coordinator.snapshot(org, None)` and looks for a node with `is_local:
// true` in the result, but `ReplicationManager::snapshot` derives its
// entire `nodes` list from partitions ALREADY in `self.registry` — which
// is itself only ever populated by `apply_assignment`, i.e. by an
// assignment this exact code path is supposed to be the one proposing.
// Empty registry -> empty snapshot -> `local_node_id` is never found ->
// `placement` stays `None` -> no assignment is ever proposed, regardless
// of whether `replication_factor` was explicit or auto-computed. This
// test therefore proposes the initial `PartitionAssignment` directly
// through `bus::replication::assignment::SqliteLedgerAssignmentStore`
// (real ledger capture, real materializer, real assignment-poll loop —
// everything DOWNSTREAM of `create_topic`'s broken bootstrap step is
// exercised for real) via the `ASSIGN` command below. This test also calls
// `CREATE_TOPIC` on all three nodes individually, which since 30.08 is
// belt-and-braces rather than the only way the row can exist: `create_topic`
// now mints a real `core.bus_topic` ledger op (`repository.rs`'s
// `CoreSyncResourceKind::BusTopic` capture, added under agent G2's extended
// wave-3 mandate — the receiving side `apply_bus_topic` had been wired and
// dead for two waves), so one node's topic row does reach the others. Two
// consequences, both measured in this file's runs and NEITHER fixed here:
//   * the "a replica has no local topic row" story behind an `isr=1` refusal
//     is now closed from the topic side, and
//   * a `test`-environment node that `CREATE_TOPIC`s the same topic name
//     makes the `prod` leader refuse its own publish with `TF3BUS ERR topic
//     environment test does not match node environment prod; fail-closed per
//     PLAN §4.4 (Z12)` (measured 30.08 18:47) — the fence working exactly as
//     designed against a harness that assumed topic rows never cross. The
//     Z12 scenario needs rethinking (create the topic on the same-environment
//     pair only, then assert the fenced node stays empty) and that belongs to
//     the wave owning the new capture, not to this file.
//
// DEFECTS FOUND WHILE RUNNING THIS TEST (statuses as of the 30.08 wave-3 close):
//
// (1) FIXED, by agent G2: `ReplicationManager::accept_stream` consumed the
// stream's ONE `ReplHello` frame itself, then handed the stream to
// `FollowerRunnerFactory::spawn` -> `follower::run_follower_stream`, which
// tried to read Hello AGAIN — so the follower never sent a `ReplAck` and
// every `acks=quorum` publish timed out with `acked=1, required=2`. The fix
// threads the already-read Hello through
// (`FollowerRunnerFactory::spawn(.., hello: ReplHello, ..)` /
// `run_follower_stream_with_hello`). VERIFIED FIXED by this test: after it
// landed, the followers do complete the handshake, reach
// `PartitionRole::Follower`, and appear in the leader's LIVE ISR.
//
// (2) WAS the wave's central blocker; FIXED by agent G2 on 30.08 (~17:26).
// `PartitionLeader::recompute_hw` sets hw = nth_largest(isr_leos, required)
// and `GlueLeaderFactory::spawn` puts the leader's own partition under
// `HwTracking::Manual`, while `leader::feed()` used to read batches through
// `fetch_raw_from_offset`, which bounds reads at `high_watermark`
// (`tentaflow-bus/src/partition.rs`). Followers start at leo 0, so
// required=2 of RF=3 gave hw=0, so the feed sent nothing, so the followers
// never advanced off leo 0, so hw stayed 0: a circular wait, reproducible
// with no iroh at all (`benches/bus_replication.rs` P6 and agent G2's
// `tests/bus_replication_three_node.rs` both hit it over an in-process
// duplex transport, so it was never a transport or harness artifact).
// `acks=leader` masked it completely (required=1 is satisfied by the leader's
// own leo, so hw == leo and the feed runs) — that is why the pre-existing
// green replication tests never caught it. `feed()` now reads through
// `PartitionReader::fetch_raw_to_end_of_log`, whose bound is
// `log_end_offset`, with an engine regression named
// `fetch_raw_to_end_of_log_feeds_what_the_high_watermark_bound_hides`.
// VERIFIED FIXED by this file: the chaos run at 18:15 reported
// `chaos: 6000 records acked before kill` — the first `acks=quorum` traffic
// this three-process harness has ever ACKed.
//
// (3) OPEN, and it is what keeps the `#[ignore]`d chaos test from producing a
// P8 number. After `child.kill()` (SIGKILL) of the leader, NEITHER survivor
// became a working leader within 20 s (measured 30.08 18:15:54 -> 18:16:14).
// The 20 s split into two distinct phases, and only the first one is the
// "detection is slow" story this paragraph used to tell:
//   * 16:15:54,30 -> ~16:15:57 both followers answer `not the leader for this
//     partition (leader_node_id=Some(<killed id>), leader_epoch=1)` and keep
//     dialing the dead node.
//   * From 16:15:58,230 (node c) and 16:15:58,385 (node b) the refusals carry
//     `leader_epoch=2` — and the id each names is the OTHER survivor (c says
//     `b1261a46…` = b, b says `c385b83f…` = c). Those are the two children's
//     own `local iroh identity` ids from this run, so both nodes DID
//     self-elect at the same epoch, both rows reached the other node, and
//     each node then treats the peer's row as the authoritative one. Neither
//     ever publishes, because neither believes it is the leader.
//   * That state is stable: 104 further refusals from b and 105 from c until
//     the window closes at 16:16:14, no progress, no lease/election line at
//     INFO or WARN from either child (only QUIC path noise).
// So the detection budget is NOT the gate — ~3,9 s (c) / ~4,1 s (b) from kill
// to an epoch-2 view fits inside the 3 s lease + 0,5 s `check_leases` poll
// (`follower.rs`, `init.rs`, and `glue.rs` now flips `lease_expired` on a
// transport-error stream exit too). What is missing is conflict resolution
// between two same-epoch assignments: there is no tie-break and no
// reconciliation that would let either node conclude it holds the only valid
// claim. Promotion after a GRACEFUL leader stop is green in
// `tests/bus_replication_three_node.rs`; a peer that dies without closing its
// connection is precisely the case where both sides race to fill the vacuum.
// One wrinkle for whoever owns this, because it changes which file to open:
// `core_materializer::apply_bus_partition_assignment` ALREADY has a
// same-epoch tie-break (`core_materializer.rs:2805-2813`): admit only if
// `incoming.leader_epoch > stored` or (`==` and
// `incoming.leader_node_id < stored_leader`). Under that rule node b
// (`b1261a46…`) must REJECT c's same-epoch row (`c385b83f…`, higher) and keep
// itself as leader; the run shows the opposite on both nodes. So either the
// rule postdates this binary (mtimes cannot settle it — the file moved at
// 18:45:30 and 18:50:49, after the 18:12 build), or a self-election writes
// its own assignment through a path that does not go through that admission
// gate. "No tie-break exists" and "the tie-break is bypassed" are different
// fixes; this harness cannot tell them apart and deliberately does not guess.
// Reported rather than fixed: `src/bus/replication/**` (incl. `election.rs`)
// and `src/sync/**` are not this file's grant.
//
// P8 consequence for the next wave: the harness cannot be blamed for the
// missing number. `first_ack_at` never fires because there is no leader to
// ACK, not because the probe window is too short (it now probes 60 s while
// asserting the 20 s budget, so an 8-20 s failover reads as a clean FAIL).
// Re-run this scenario only after the epoch-conflict resolution lands.
//
// Second reproduction (run started 19:19 local, kill at 17:19:50,403Z, exe built
// 19:15:09) with the 60 s window shows the same stable epoch-2 draw for ~43 s and
// then a SECOND failure
// mode the 20 s window could not see: at 17:20:41 node b answers PUBLISH_BATCH as
// a leader (`not enough in-sync replicas: isr=1, required=2`), and node c answers
// its own probe as a leader too, 30,03 s after that probe, with `timed out
// waiting for replica acks: acked=1, required=2` — its full
// `DEFAULT_PUBLISH_ACK_TIMEOUT`. So promotion is not exclusive here either: both
// survivors reach Leader and neither can form an ISR, while both sit in a
// symmetric `leader follower-stream ended, reconnecting` loop whose Hello the
// peer refuses with `NotAReplica` — each dials the other as leader while
// rejecting the other's dial as a follower. Consequence for whoever picks this
// up: a same-epoch tie-break alone cannot produce a P8 pass, because the node it
// promotes still has to (re)form the ISR, and the `NotAReplica` refusal is where
// that dies. Reported, not fixed: `src/bus/replication/**`.
//
// Timing provenance for any future P8 number from this file: until 30.08 19:45
// the parent-side probe consulted its deadline only BETWEEN blocking `read_line`
// calls, so a child that answered late stalled the loop (8,18 s on a 700 ms
// publish budget in the run above) and the late reply was then consumed as the
// answer to the NEXT command — the 17:20:33 -> 17:21:03 pair is that exact
// desync. `next_line` now waits on a channel with `recv_timeout`, `try_command`
// discards exactly one stale reply, and the `/bin/sh` tests below pin both
// behaviours plus EOF and the READY banner. Failover timings from before 19:45
// are suspect in this specific way and were never banked as measurements.
//
// A SETUP RACE this file used to lose, and which reads exactly like a
// replication defect: `ROLE` reports LOCAL intent (this node's registry
// entry), which converges strictly BEFORE the leader's live ISR does. An
// independent run of this file on 30.08 measured all three roles converged
// at 16:05:04.221156 and the publish issued 0.5 ms later was refused with
// `not enough in-sync replicas: isr=1, required=2` — the replicas had the
// assignment, the leader had not yet gotten a Hello accepted by them.
// `preflight`'s `NotEnoughReplicas` gate reads the LIVE set, so
// `assign_and_wait` now waits for `wait_live_isr` (the leader's
// `snapshot(...).isr`, live only when asked of the leader) in addition to
// the per-node roles, and the Z12 test waits for the healthy pair the same
// way.
//
// Every command/plumbing choice in this file (self-materializing `ASSIGN`
// instead of racing `apply_assignment`, the `sync_nodes`/`sync_policies`/
// environment-stamp seeding in `TRUST`/`CONNECT`, the ALPN_BUS contact-hint
// fix in `CONNECT`) was verified correct up to and including both followers
// reaching `PartitionRole::Follower` and joining the live ISR.
// =============================================================================

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use sha2::{Digest, Sha256};

use tentaflow_core::bus::replication::assignment::{
    PartitionAssignment, SqliteLedgerAssignmentStore,
};
use tentaflow_core::bus::replication::glue::PartitionProvider;
use tentaflow_core::bus::replication::init::{init as replication_init, ReplicationInitConfig};
use tentaflow_core::bus::topics::{Acks, DurabilityClass, TopicOptions};
use tentaflow_core::bus::{
    self, BusAction, BusCallContext, BusInitConfig, BusServiceError, PublishBatch, PublishRecord,
};
use tentaflow_core::mesh::iroh_manager::{IrohMeshConfig, IrohMeshEvent, IrohMeshManager};
use tentaflow_core::mesh::security::MeshSecurity;
use tentaflow_protocol::environment::NodeEnvironment;

// "org-default": the seeded default org present in every fresh DB
// (`db::migrations`'s org-default seed step). A freshly-invented org id
// hits a foreign-key failure on `sync_policies.org_id` (no matching
// `organizations` row) — matching `tests/process_four_node_sync.rs`'s own
// choice of `"org-default"` for every `CoreWriteCapture`/`upsert_sync_policy`
// call in that file.
const ORG_ID: &str = "org-default";
const TOPIC: &str = "chaos.orders";
const PARTITION: u32 = 0;
const RECORD_BYTES: usize = 1024;
const BATCH_RECORDS: usize = 1000;

/// Matches the tracing subscriber's own timestamp format (RFC3339 UTC) so
/// parent-side command logs can be correlated by eye against child-side
/// `tracing` output in `--nocapture` runs.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}

// ===== Allow-all authorizer (identical shape to `tests/bus_demo_seed.rs`'s
// harness authorizer — RBAC is out of scope for a replication chaos test) ===

struct AllowAllAuthorizer;

impl bus::BusAuthorizer for AllowAllAuthorizer {
    fn authorize(
        &self,
        _ctx: &BusCallContext,
        _action: BusAction,
        _topic: &str,
    ) -> Result<(), BusServiceError> {
        Ok(())
    }

    fn authorize_group(
        &self,
        _ctx: &BusCallContext,
        _action: BusAction,
        _topic: &str,
        _group: &str,
    ) -> Result<(), BusServiceError> {
        Ok(())
    }

    fn generation(&self) -> u64 {
        0
    }
}

// =============================================================================
// Parent side
// =============================================================================

struct ChildNode {
    child: Child,
    stdin: ChildStdin,
    /// One line per child stdout line, fed by the pump thread started in
    /// `from_child`. Deliberately NOT a `BufReader<ChildStdout>` field: a
    /// blocking `read_line` cannot honour a deadline, and the P8 probe loop
    /// needs it to (see `try_command`).
    lines: Receiver<String>,
    /// Set when a command timed out while the child was still working. The
    /// protocol reply that eventually arrives belongs to THAT command, not to
    /// the next one, so `try_command` discards exactly one such line.
    stale_reply_pending: bool,
    stderr_lines: Arc<Mutex<Vec<String>>>,
    node_id: String,
    public_key: String,
    addr: SocketAddr,
    name: String,
}

impl ChildNode {
    fn from_child(mut child: Child, name: &str) -> Self {
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");
        let (tx, lines) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    continue;
                }
            }
        });
        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        {
            let buf = Arc::clone(&stderr_lines);
            let tag = name.to_string();
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    eprintln!("TF3BUS[{tag}] {line}");
                    buf.lock().unwrap().push(line);
                }
            });
        }
        Self {
            child,
            stdin,
            lines,
            stale_reply_pending: false,
            stderr_lines,
            node_id: String::new(),
            public_key: String::new(),
            addr: "127.0.0.1:0".parse().expect("addr"),
            name: name.to_string(),
        }
    }

    fn spawn(name: &str, home: PathBuf) -> Self {
        Self::spawn_with_env(name, home, &[])
    }

    fn spawn_with_env(name: &str, home: PathBuf, extra_env: &[(&str, &str)]) -> Self {
        let exe = std::env::current_exe().expect("current test exe");
        let mut cmd = Command::new(exe);
        cmd.arg("--exact")
            .arg("process_three_node_bus_child")
            .arg("--nocapture")
            .env("TENTAFLOW_BUS_CHAOS_CHILD", "1")
            .env("TENTAFLOW_BUS_CHAOS_HOME", &home)
            .env("TENTAFLOW_BUS_CHAOS_NAME", name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawn child node");
        let mut node = Self::from_child(child, name);
        let ready = node.read_prefixed_line("READY", Duration::from_secs(20));
        let parts = ready.split_whitespace().collect::<Vec<_>>();
        assert_eq!(
            parts.len(),
            5,
            "READY line must include node_id public_key addr, got: {ready}"
        );
        node.node_id = parts[2].to_string();
        node.public_key = parts[3].to_string();
        node.addr = parts[4].parse().expect("ready addr");
        node
    }

    fn command(&mut self, command: &str) -> String {
        self.command_timeout(command, Duration::from_secs(30))
    }

    fn command_timeout(&mut self, command: &str, timeout: Duration) -> String {
        match self.try_command(command, timeout) {
            Ok(line) => line,
            Err(e) => panic!("child command failed: {command}: {e}"),
        }
    }

    /// Outcome of waiting for the next line from the child's stdout.
    fn next_line(&self, deadline: Instant) -> Result<String, LineWait> {
        let remaining = match deadline.checked_duration_since(Instant::now()) {
            Some(r) if !r.is_zero() => r,
            _ => return Err(LineWait::Deadline),
        };
        match self.lines.recv_timeout(remaining) {
            Ok(line) => Ok(line),
            Err(RecvTimeoutError::Timeout) => Err(LineWait::Deadline),
            Err(RecvTimeoutError::Disconnected) => Err(LineWait::Eof),
        }
    }

    fn is_protocol_reply(line: &str) -> bool {
        line.starts_with("TF3BUS OK") || line.starts_with("TF3BUS ERR")
    }

    fn try_command(&mut self, command: &str, timeout: Duration) -> Result<String, String> {
        eprintln!(
            "TF3BUS {} parent -> {}: {command}",
            now_rfc3339(),
            self.name
        );
        writeln!(self.stdin, "{command}").map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())?;
        let deadline = Instant::now() + timeout;
        loop {
            let line = match self.next_line(deadline) {
                Ok(line) => line,
                Err(LineWait::Deadline) => {
                    self.stale_reply_pending = true;
                    return Err(format!("timeout waiting for response to {command}"));
                }
                Err(LineWait::Eof) => {
                    return Err(format!("child exited during command: {command}"))
                }
            };
            let trimmed = line.trim();
            if self.stale_reply_pending && Self::is_protocol_reply(trimmed) {
                self.stale_reply_pending = false;
                eprintln!(
                    "TF3BUS {} parent ~~ {}: {trimmed} (late reply to an already-timed-out command, discarded)",
                    now_rfc3339(),
                    self.name
                );
                continue;
            }
            if trimmed.starts_with("TF3BUS OK") {
                eprintln!(
                    "TF3BUS {} parent <- {}: {trimmed}",
                    now_rfc3339(),
                    self.name
                );
                return Ok(trimmed.to_string());
            }
            if trimmed.starts_with("TF3BUS ERR") {
                eprintln!(
                    "TF3BUS {} parent <- {}: {trimmed}",
                    now_rfc3339(),
                    self.name
                );
                return Err(trimmed.to_string());
            }
        }
    }

    fn read_prefixed_line(&mut self, prefix: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let needle = format!("TF3BUS {prefix}");
        loop {
            let line = match self.next_line(deadline) {
                Ok(line) => line,
                Err(LineWait::Deadline) => panic!("timeout waiting for {prefix}"),
                Err(LineWait::Eof) => panic!("child exited before {prefix}"),
            };
            let trimmed = line.trim();
            if trimmed.starts_with(&needle) {
                return trimmed.to_string();
            }
        }
    }

    fn stderr_contains(&self, needle: &str) -> bool {
        self.stderr_lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains(needle))
    }

    /// SIGKILL — the whole point of this test (`Drop`ping a `BusService`
    /// cannot exercise `kill -9` semantics; see this file's module doc for
    /// why this test cannot be in-process).
    fn kill(&mut self) {
        self.child.kill().expect("SIGKILL leader");
        let _ = self.child.wait();
    }
}

/// Why a wait for the next child line ended. Kept separate from the command
/// result so both readers can share one timeout implementation.
enum LineWait {
    Deadline,
    Eof,
}

impl Drop for ChildNode {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "STOP");
        let _ = self.stdin.flush();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Harness self-checks: the parent-side probe enforces its own deadline.
//
// These are not part of the three-node scenario. They exist because the probe
// loop used to check `Instant::now() > deadline` only *between* two blocking
// `read_line` calls, so a child that took 8,18 s to answer a 700 ms budget
// stalled the P8 test far past its window and made the measured failover time
// untrustworthy (M2-WYNIKI.md, "Defekt sondy"). A deadline is only real if it
// can interrupt the read, so `next_line` waits on a channel with a timeout and
// the tests below pin both that and the discard of the late reply.
// ---------------------------------------------------------------------------

/// A node-shaped handle over `/bin/sh`, so a test can control exactly when a
/// reply line appears without involving the TentaFlow child protocol.
fn fake_shell_node() -> ChildNode {
    let mut cmd = Command::new("/bin/sh");
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn /bin/sh");
    ChildNode::from_child(child, "probe")
}

#[test]
fn timeout_is_enforced_inside_the_read_and_late_reply_is_discarded() {
    let mut node = fake_shell_node();

    // (1) The budget must be honoured *while* waiting for the line. Before
    // the pump existed, `read_line` blocked until the child answered, so this
    // call returned ~500 ms after the request instead of after ~150 ms.
    let started = Instant::now();
    let err = node
        .try_command(
            "sleep 0.5; echo 'TF3BUS OK LATE'",
            Duration::from_millis(150),
        )
        .expect_err("a child that answers after the budget must time out");
    let timed_out_at = started.elapsed();
    assert!(err.contains("timeout"), "unexpected error: {err}");
    assert!(
        timed_out_at < Duration::from_millis(450),
        "deadline not enforced inside the read: waited {timed_out_at:?} on a 150 ms budget"
    );

    // (2) The LATE line still arrives (~350 ms into this call, while the
    // child is already past its sleep). It belongs to the command that timed
    // out and must not be returned as the answer to *this* one.
    let started = Instant::now();
    let reply = node
        .try_command("echo 'TF3BUS OK NEXT'", Duration::from_secs(5))
        .expect("the shell answers the second command");
    let second_at = started.elapsed();
    assert_eq!(reply, "TF3BUS OK NEXT");
    assert!(
        second_at < Duration::from_millis(900),
        "stale-reply handling waited far longer than the child's own sleep: {second_at:?}"
    );

    node.kill();
}

#[test]
fn child_exit_is_reported_as_eof_not_as_a_hang() {
    let mut node = fake_shell_node();
    let err = node
        .try_command("echo 'TF3BUS BYE'; exit 0", Duration::from_secs(5))
        .expect_err("a non-protocol line is not a reply, and the child then exits");
    assert!(
        err.contains("child exited") || err.contains("TF3BUS ERR"),
        "unexpected error: {err}"
    );
}

#[test]
fn read_prefixed_line_finds_the_banner_after_noise() {
    let mut node = fake_shell_node();
    writeln!(node.stdin, "echo noise").expect("write");
    writeln!(node.stdin, "echo 'TF3BUS READY nodekey 127.0.0.1:9'").expect("write");
    node.stdin.flush().expect("flush");
    let ready = node.read_prefixed_line("READY", Duration::from_secs(5));
    assert!(ready.starts_with("TF3BUS READY"), "got: {ready}");
    node.kill();
}

#[test]
#[should_panic(expected = "timeout waiting for READY")]
fn read_prefixed_line_also_honours_its_budget() {
    let mut node = fake_shell_node();
    // A child that never prints `READY` used to hang this call for the whole
    // life of the process, because the deadline was only consulted between
    // reads and `read_line` blocks.
    node.read_prefixed_line("READY", Duration::from_millis(300));
}

fn connect_nodes(a: &mut ChildNode, b: &mut ChildNode) {
    a.command(&format!("TRUST {} {}", b.node_id, b.public_key));
    b.command(&format!("TRUST {} {}", a.node_id, a.public_key));
    a.command(&format!(
        "CONNECT {} {} {}",
        b.node_id, b.public_key, b.addr
    ));
    b.command(&format!(
        "CONNECT {} {} {}",
        a.node_id, a.public_key, a.addr
    ));
    // `sync::runtime::target_environment_allowed` (ROADMAP Z12 P2-2) fails
    // CLOSED — never a same-environment match — when a trusted peer's
    // environment is unstamped, so EVERY ledger op (including `core.
    // bus_partition_assignment`) silently never queues an outbox target for
    // a peer with no `trusted_nodes.environment` row, regardless of the
    // `sync_policies`/`sync_nodes` setup above being otherwise correct
    // (found the hard way — no warning surfaces anywhere above `debug!`).
    // Production stamps this at real PIN-pairing confirm time
    // (`MeshSecurity::confirm_pairing`); this harness pairs via `TRUST`/
    // `CONNECT` instead, so both directions are stamped `prod` (this
    // harness's default `NodeEnvironment`) here — the Z12 test overrides
    // this for its env-mismatched node afterward.
    a.command(&format!("SET_PEER_ENV {} prod", b.node_id));
    b.command(&format!("SET_PEER_ENV {} prod", a.node_id));
}

fn create_topic_on(node: &mut ChildNode, acks: &str, durability: &str) {
    node.command(&format!(
        "CREATE_TOPIC {ORG_ID} {TOPIC} 1 3 {acks} {durability}"
    ));
}

fn role_of(node: &mut ChildNode) -> String {
    node.command(&format!("ROLE {ORG_ID} {TOPIC} {PARTITION}"))
}

/// The live in-sync replica set for the harness partition, read from `node`
/// (only the leader's answer is a convergence signal — a non-leader reports
/// the last materialized `assignment.isr`, not what the leader currently
/// holds).
fn live_isr(node: &mut ChildNode) -> Vec<String> {
    let line = node.command(&format!("ISR {ORG_ID} {TOPIC} {PARTITION}"));
    // "TF3BUS OK ISR <count> [<member>,<member>…]" — same fixed-index shape as
    // the other child replies in this file.
    let parts: Vec<&str> = line.split_whitespace().collect();
    let count: usize = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    match parts.get(4) {
        Some(members) if count > 0 => members.split(',').map(|s| s.to_string()).collect(),
        _ => Vec::new(),
    }
}

/// Blocks until the leader's live ISR covers every id in `want`, or the
/// timeout is spent.
fn wait_live_isr(node: &mut ChildNode, want: &[String], timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let isr = live_isr(node);
        if want.iter().all(|n| isr.contains(n)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "live ISR never covered {want:?} (last seen {isr:?}) within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_role(node: &mut ChildNode, wants: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let line = role_of(node);
        if line.contains(wants) {
            return line;
        }
        assert!(
            Instant::now() < deadline,
            "{} did not reach role {wants} within {timeout:?}: last={line}",
            node.name
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// One `PUBLISH_BATCH` call = 1000 records x 1 KiB (PLAN-M2 §1g's own
/// numbers). Returns `(base_offset, accepted, hw)` on success.
fn publish_batch(node: &mut ChildNode, timeout: Duration) -> Result<(u64, u32, u64), String> {
    let line = node.try_command(
        &format!("PUBLISH_BATCH {ORG_ID} {TOPIC} {BATCH_RECORDS} {RECORD_BYTES}"),
        timeout,
    )?;
    // "TF3BUS OK PUBLISH_BATCH <base_offset> <accepted> <hw> <elapsed_us>"
    let parts: Vec<&str> = line.split_whitespace().collect();
    let base_offset: u64 = parts[3].parse().map_err(|e| format!("{e}"))?;
    let accepted: u32 = parts[4].parse().map_err(|e| format!("{e}"))?;
    let hw: u64 = parts[5].parse().map_err(|e| format!("{e}"))?;
    Ok((base_offset, accepted, hw))
}

fn hash_log(node: &mut ChildNode, upto_offset: u64) -> (String, u64) {
    let line = node.command(&format!(
        "HASH_LOG {ORG_ID} {TOPIC} {PARTITION} {upto_offset}"
    ));
    // "TF3BUS OK HASH_LOG <sha256hex> <count>"
    let parts: Vec<&str> = line.split_whitespace().collect();
    (parts[3].to_string(), parts[4].parse().expect("count"))
}

fn partition_stats(node: &mut ChildNode) -> (u64, u64, u64) {
    // `ChildNode::command` hands back the WHOLE reply line, so the token
    // layout is "TF3BUS OK STATS <earliest> <hw> <leo>" — index 3 is the
    // first field, same convention as `publish_batch` and `hash_log`. This
    // helper only runs after a publish succeeds, which is why the offset
    // stayed hidden while every wave of this file died at the publish step:
    // parsed at index 2 it hands back "STATS" and panics in `parse`.
    let line = node.command(&format!("STATS {ORG_ID} {TOPIC} {PARTITION}"));
    let parts: Vec<&str> = line.split_whitespace().collect();
    (
        parts[3].parse().expect("earliest"),
        parts[4].parse().expect("hw"),
        parts[5].parse().expect("leo"),
    )
}

/// Proposes the initial `PartitionAssignment` through the real ledger (see
/// the module doc's "KNOWN GAP" section for why this cannot go through
/// `create_topic` itself), then waits until every node in `replicas`
/// reports the expected role via `ROLE`.
fn assign_and_wait(
    proposer: &mut ChildNode,
    others: &mut [&mut ChildNode],
    leader_node_id: &str,
    replicas: &[String],
    timeout: Duration,
) {
    proposer.command(&format!(
        "ASSIGN {ORG_ID} {TOPIC} {PARTITION} {leader_node_id} {}",
        replicas.join(",")
    ));
    let deadline = Instant::now() + timeout;
    let expect = |n: &mut ChildNode| -> bool {
        if !replicas.contains(&n.node_id) {
            return true;
        }
        let want_leader = n.node_id == leader_node_id;
        let line = role_of(n);
        (want_leader && line.contains("Leader")) || (!want_leader && line.contains("Follower"))
    };
    loop {
        let mut ok = expect(proposer);
        for n in others.iter_mut() {
            ok &= expect(n);
        }
        if ok {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "assignment did not converge within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(150));
    }
    // Local role convergence is NOT the gate a publish is checked against.
    // `preflight` refuses on the leader's LIVE ISR, which only fills in as
    // the leader's dial reaches each replica and its Hello is accepted —
    // measurably ~0.5 ms later than the last `ROLE Follower` in a low-load
    // run (observed in an independent run of this file: all three roles
    // converged at 16:05:04,221156 and the publish 0.5 ms later was refused
    // with `isr=1, required=2`). Without this second wait the test races
    // its own setup and the failure looks like a replication defect.
    wait_live_isr(proposer, replicas, timeout);
}

// ===== Smoke test (non-ignored, CI-safe, <= 90s) =============================
//
// Boots 3 nodes, pairs them, creates an RF=3 topic (acks=quorum, class
// standard), and verifies 100 records replicate to all 3 nodes' logs
// (byte-for-byte, via HASH_LOG) with no leader kill — the fast, always-run
// half of PLAN-M2 §1g's "two levels" split.
#[test]
fn process_three_node_bus_failover_smoke() {
    if std::env::var_os("TENTAFLOW_BUS_CHAOS_CHILD").is_some() {
        return;
    }
    let start = Instant::now();
    let root = tempfile::tempdir().expect("root");

    let mut a = ChildNode::spawn("a", root.path().join("a"));
    let mut b = ChildNode::spawn("b", root.path().join("b"));
    let mut c = ChildNode::spawn("c", root.path().join("c"));

    connect_nodes(&mut a, &mut b);
    connect_nodes(&mut a, &mut c);
    connect_nodes(&mut b, &mut c);

    create_topic_on(&mut a, "quorum", "standard");
    create_topic_on(&mut b, "quorum", "standard");
    create_topic_on(&mut c, "quorum", "standard");

    let leader_id = a.node_id.clone();
    let replicas = vec![a.node_id.clone(), b.node_id.clone(), c.node_id.clone()];
    assign_and_wait(
        &mut a,
        &mut [&mut b, &mut c],
        &leader_id,
        &replicas,
        Duration::from_secs(30),
    );

    // Only 100 records for the smoke test (task spec: "100 records"),
    // sent as a single `PUBLISH_BATCH` (this harness's unit is 1000
    // records; a smaller batch just uses fewer per-record bytes so the
    // total stays close to the spec — the important gate here is
    // replication correctness, not throughput).
    let line = a
        .try_command(
            &format!("PUBLISH_BATCH {ORG_ID} {TOPIC} 100 {RECORD_BYTES}"),
            Duration::from_secs(10),
        )
        .expect("publish on leader");
    let parts: Vec<&str> = line.split_whitespace().collect();
    let base_offset: u64 = parts[3].parse().unwrap();
    let accepted: u32 = parts[4].parse().unwrap();
    let hw: u64 = parts[5].parse().unwrap();
    assert_eq!(accepted, 100);
    assert!(hw >= base_offset + accepted as u64);
    let total = base_offset + accepted as u64;

    for node in [&mut b, &mut c] {
        loop {
            let (_, hw, _) = partition_stats(node);
            if hw >= total {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "{} did not reach hw={total} within 60s (smoke test budget)",
                node.name
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    let (hash_a, count_a) = hash_log(&mut a, total);
    let (hash_b, count_b) = hash_log(&mut b, total);
    let (hash_c, count_c) = hash_log(&mut c, total);
    assert_eq!(count_a, total, "leader record count mismatch");
    assert_eq!(count_a, count_b, "leader/follower-b record count mismatch");
    assert_eq!(count_a, count_c, "leader/follower-c record count mismatch");
    assert_eq!(hash_a, hash_b, "leader/follower-b log hash mismatch");
    assert_eq!(hash_a, hash_c, "leader/follower-c log hash mismatch");

    eprintln!(
        "process_three_node_bus_failover_smoke: OK in {:?} ({total} records replicated to 3/3 nodes, hashes equal)",
        start.elapsed()
    );
    assert!(
        start.elapsed() < Duration::from_secs(90),
        "smoke test exceeded its 90s budget: {:?}",
        start.elapsed()
    );
}

// ===== Z12: environment fencing on the real ALPN (non-ignored, fast) ========
//
// Node C declares `node_environment = Test` while A/B are `Prod` (the
// default). C is deliberately listed in the partition's `replicas`/`isr` (a
// misconfigured admin action, or a node whose environment changed after
// assignment) so the leader (A) actually attempts to dial it on ALPN_BUS —
// proving the rejection happens at the real accept gate
// (`mesh/iroh_manager.rs`'s `bus_accept_env_check`), not merely because
// `create_topic`'s env filter kept C out of the replica set to begin with.
#[test]
fn process_three_node_bus_failover_z12_environment_fencing() {
    if std::env::var_os("TENTAFLOW_BUS_CHAOS_CHILD").is_some() {
        return;
    }
    let root = tempfile::tempdir().expect("root");

    let mut a = ChildNode::spawn("a", root.path().join("a"));
    let mut b = ChildNode::spawn("b", root.path().join("b"));
    let mut c = ChildNode::spawn_with_env(
        "c",
        root.path().join("c"),
        &[("TENTAFLOW_BUS_CHAOS_ENV", "test")],
    );

    connect_nodes(&mut a, &mut b);
    connect_nodes(&mut a, &mut c);
    connect_nodes(&mut b, &mut c);

    // Gate (a) in `mesh/iroh_manager.rs` (`bus_accept_env_check`) reads
    // EACH node's own `trusted_nodes.environment` column for the remote
    // peer — populated at real pairing-confirm time in production
    // (`MeshSecurity::confirm_pairing`), stamped directly here since this
    // harness pairs via `TRUST`/`CONNECT`, not the PIN flow.
    a.command(&format!("SET_PEER_ENV {} test", c.node_id));
    b.command(&format!("SET_PEER_ENV {} test", c.node_id));
    c.command(&format!("SET_PEER_ENV {} prod", a.node_id));
    c.command(&format!("SET_PEER_ENV {} prod", b.node_id));

    create_topic_on(&mut a, "quorum", "standard");
    create_topic_on(&mut b, "quorum", "standard");
    create_topic_on(&mut c, "quorum", "standard");

    let leader_id = a.node_id.clone();
    let replicas = vec![a.node_id.clone(), b.node_id.clone(), c.node_id.clone()];
    // C is included on purpose (see test doc). Only A/B's roles are waited
    // on here — C's outcome is asserted separately below as a functional
    // proof, not merely a role label (role assignment is local intent, not
    // proof of a live replication stream).
    a.command(&format!(
        "ASSIGN {ORG_ID} {TOPIC} {PARTITION} {leader_id} {}",
        replicas.join(",")
    ));
    wait_role(&mut a, "Leader", Duration::from_secs(15));
    wait_role(&mut b, "Follower", Duration::from_secs(15));
    // Same reason as in `assign_and_wait`: publish only after the leader's
    // live ISR actually covers the healthy pair, so a refusal here is a
    // replication result rather than a setup race. C is deliberately NOT in
    // the expected set — the env gate must keep it out, which is asserted
    // functionally below.
    let healthy: Vec<String> = vec![a.node_id.clone(), b.node_id.clone()];
    wait_live_isr(&mut a, &healthy, Duration::from_secs(20));

    let line = a
        .try_command(
            &format!("PUBLISH_BATCH {ORG_ID} {TOPIC} {BATCH_RECORDS} {RECORD_BYTES}"),
            Duration::from_secs(10),
        )
        .expect("publish on leader");
    let accepted: u32 = line.split_whitespace().nth(4).unwrap().parse().unwrap();
    assert_eq!(accepted, BATCH_RECORDS as u32);

    // Give A's dial-to-C attempt and B's replication plenty of time to
    // settle, then assert the FUNCTIONAL outcome: B (legit same-env
    // follower) caught up, C (env-fenced) never received a single byte.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, hw_b, _) = partition_stats(&mut b);
        if hw_b >= accepted as u64 {
            break;
        }
        assert!(Instant::now() < deadline, "follower B never caught up");
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, hw_c, leo_c) = partition_stats(&mut c);
    assert_eq!(
        (hw_c, leo_c),
        (0, 0),
        "Z12 VIOLATION: env-fenced node C received replicated data (hw={hw_c}, leo={leo_c})"
    );

    // Evidence: the rejection log line, from whichever side observed it
    // (C's accept-side `bus_accept_env_check` warn, or A's dial-side "dial
    // follower failed" warn — either proves the handshake never completed).
    let rejected = c.stderr_contains("env mismatch")
        || c.stderr_contains("env-mismatch")
        || a.stderr_contains("dial follower failed")
        || a.stderr_contains("env-mismatch");
    assert!(
        rejected,
        "expected an env-mismatch/dial-failed log line on node A or C; \
         A stderr={:?} C stderr={:?}",
        a.stderr_lines.lock().unwrap(),
        c.stderr_lines.lock().unwrap()
    );

    eprintln!(
        "process_three_node_bus_failover_z12_environment_fencing: OK — C never joined ISR, rejection logged"
    );
}

// ===== Chaos test (ignored — PLAN-M2 §1g's real gate) ========================
//
// `cargo test --test process_three_node_bus_failover -- --ignored --nocapture`
#[test]
#[ignore]
fn process_three_node_bus_failover_chaos() {
    if std::env::var_os("TENTAFLOW_BUS_CHAOS_CHILD").is_some() {
        return;
    }
    let root = tempfile::tempdir().expect("root");
    let home_a = root.path().join("a");
    let home_b = root.path().join("b");
    let home_c = root.path().join("c");

    let mut a = ChildNode::spawn("a", home_a.clone());
    let mut b = ChildNode::spawn("b", home_b.clone());
    let mut c = ChildNode::spawn("c", home_c.clone());

    connect_nodes(&mut a, &mut b);
    connect_nodes(&mut a, &mut c);
    connect_nodes(&mut b, &mut c);

    create_topic_on(&mut a, "quorum", "standard");
    create_topic_on(&mut b, "quorum", "standard");
    create_topic_on(&mut c, "quorum", "standard");

    let leader_id = a.node_id.clone();
    let replicas = vec![a.node_id.clone(), b.node_id.clone(), c.node_id.clone()];
    assign_and_wait(
        &mut a,
        &mut [&mut b, &mut c],
        &leader_id,
        &replicas,
        Duration::from_secs(30),
    );

    // ---- Phase 1: steady-state load on the leader (a) --------------------
    let mut total_acked: u64 = 0;
    let mut last_ack_at = Instant::now();
    let load_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < load_deadline {
        match publish_batch(&mut a, Duration::from_secs(5)) {
            Ok((base_offset, accepted, _hw)) => {
                total_acked = base_offset + accepted as u64;
                last_ack_at = Instant::now();
            }
            Err(e) => panic!("unexpected publish failure before kill: {e}"),
        }
    }
    assert!(total_acked > 0, "no records acked before kill");
    eprintln!("chaos: {total_acked} records acked before kill");

    // ---- Phase 2: SIGKILL the leader, measure P8 --------------------------
    a.kill();
    eprintln!("chaos: leader SIGKILLed");

    // Whichever of b/c is promoted "wins" — this harness does not assume
    // which one (K-M2-3's tie-break is lowest node_id among equal LEO, not
    // something worth predicting here).
    // The PLAN gate is <=8s; 20s is this harness's own generous budget and
    // 60s is a diagnostic probe window past it. Probing past the gate on
    // purpose: a promotion that lands at, say, 35s is a FAIL against the
    // gate but a DIFFERENT failure than one that never lands, and stopping
    // at the budget would throw that distinction away. The gate assert still
    // fires at 20s — the extra window only adds a number to the report.
    let p8_budget = Duration::from_secs(20);
    let p8_probe_window = Duration::from_secs(60);
    let kill_at = Instant::now();
    let mut first_new_leader_ack: Option<(Instant, String)> = None;
    while Instant::now().duration_since(kill_at) < p8_probe_window {
        for node in [&mut b, &mut c] {
            if let Ok((base_offset, accepted, _hw)) =
                publish_batch(node, Duration::from_millis(700))
            {
                total_acked = total_acked.max(base_offset + accepted as u64);
                first_new_leader_ack = Some((Instant::now(), node.name.clone()));
                break;
            }
        }
        if first_new_leader_ack.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    let (first_ack_at, new_leader_name) = first_new_leader_ack.unwrap_or_else(|| {
        panic!(
            "no survivor became a working leader within {}s (P8 budget: {}s, PLAN gate: <=8s)",
            p8_probe_window.as_secs(),
            p8_budget.as_secs()
        )
    });
    let p8 = first_ack_at.duration_since(last_ack_at);
    eprintln!(
        "chaos: P8 (last ACK before kill -> first ACK from new leader {new_leader_name}) = {p8:?}          ({}s after SIGKILL; budget {}s, PLAN gate <=8s{})",
        first_ack_at.duration_since(kill_at).as_secs_f64(),
        p8_budget.as_secs(),
        if p8 > p8_budget { ", GATE EXCEEDED" } else { "" }
    );
    assert!(
        p8 <= p8_budget,
        "P8 {p8:?} exceeds the harness budget of {p8_budget:?} (PLAN gate: <=8s)"
    );

    let b_is_new_leader = new_leader_name == b.name;
    let (new_leader, other_survivor) = if b_is_new_leader {
        (&mut b, &mut c)
    } else {
        (&mut c, &mut b)
    };

    // ---- Phase 3: zero-loss check ------------------------------------------
    // Every offset ACKed before the kill must be readable (no gaps) from
    // the new leader once it finishes reconciling hw to its own leo
    // (K-M2-1: hw is monotonic — the new leader never regresses it, but may
    // take a short re-ack round trip to raise it all the way to leo).
    let acked_before_kill = total_acked; // last successful pre-kill ack point
    let catchup_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, hw, _) = partition_stats(new_leader);
        if hw >= acked_before_kill {
            break;
        }
        assert!(
            Instant::now() < catchup_deadline,
            "new leader {new_leader_name} hw never caught up to pre-kill ack point {acked_before_kill}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, count_on_new_leader) = hash_log(new_leader, acked_before_kill);
    assert_eq!(
        count_on_new_leader, acked_before_kill,
        "LOSS DETECTED: new leader is missing acked records (expected {acked_before_kill} contiguous records, got {count_on_new_leader})"
    );
    eprintln!("chaos: zero-loss check OK — {acked_before_kill} pre-kill acked records all present on new leader {new_leader_name}");

    // ---- Phase 4: keep producing through the new leader --------------------
    for _ in 0..3 {
        let (base_offset, accepted, _hw) =
            publish_batch(new_leader, Duration::from_secs(5)).expect("publish after failover");
        total_acked = total_acked.max(base_offset + accepted as u64);
    }
    eprintln!("chaos: production continued through new leader up to offset {total_acked}");

    // ---- Phase 5: restart the killed node, wait for rejoin ------------------
    drop(a);
    let mut a2 = ChildNode::spawn("a-restart", home_a);
    // Re-pair: `a2`'s mesh endpoint binds a fresh random port, so the two
    // survivors' previously-learned address for the old "a" is stale.
    // `TRUST` is already persisted on disk from before the kill (same home
    // dir => same `trusted_nodes` rows) — only re-dialing is needed.
    a2.command(&format!(
        "CONNECT {} {} {}",
        new_leader.node_id, new_leader.public_key, new_leader.addr
    ));
    a2.command(&format!(
        "CONNECT {} {} {}",
        other_survivor.node_id, other_survivor.public_key, other_survivor.addr
    ));
    new_leader.command(&format!(
        "CONNECT {} {} {}",
        a2.node_id, a2.public_key, a2.addr
    ));
    other_survivor.command(&format!(
        "CONNECT {} {} {}",
        a2.node_id, a2.public_key, a2.addr
    ));
    assert_eq!(
        a2.node_id, leader_id,
        "restarted node must keep its original identity (same home dir => same persisted keypair)"
    );

    let rejoin_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let line = role_of(&mut a2);
        if line.contains("Follower") {
            break;
        }
        assert!(
            Instant::now() < rejoin_deadline,
            "restarted node a did not rejoin as Follower within 30s: last={line}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    eprintln!("chaos: restarted node a rejoined as Follower");

    let catchup2_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, hw, _) = partition_stats(&mut a2);
        if hw >= total_acked {
            break;
        }
        assert!(
            Instant::now() < catchup2_deadline,
            "restarted node a did not catch up to hw={total_acked} within 15s"
        );
        std::thread::sleep(Duration::from_millis(150));
    }

    // ---- Phase 6: byte-for-byte log comparison up to min(hw) ---------------
    let (_, hw_leader, _) = partition_stats(new_leader);
    let (_, hw_other, _) = partition_stats(other_survivor);
    let (_, hw_a2, _) = partition_stats(&mut a2);
    let min_hw = hw_leader.min(hw_other).min(hw_a2);
    assert!(
        min_hw >= total_acked,
        "min(hw) regressed below the last known committed offset"
    );

    let (hash_leader, count_leader) = hash_log(new_leader, min_hw);
    let (hash_other, count_other) = hash_log(other_survivor, min_hw);
    let (hash_a2, count_a2) = hash_log(&mut a2, min_hw);
    assert_eq!(count_leader, min_hw);
    assert_eq!(
        count_leader, count_other,
        "record count mismatch vs other survivor"
    );
    assert_eq!(
        count_leader, count_a2,
        "record count mismatch vs rejoined node"
    );
    assert_eq!(
        hash_leader, hash_other,
        "log hash mismatch vs other survivor"
    );
    assert_eq!(hash_leader, hash_a2, "log hash mismatch vs rejoined node");

    eprintln!(
        "process_three_node_bus_failover_chaos: OK — P8={p8:?}, loss=0/{acked_before_kill}, \
         3-way log hash equal up to min(hw)={min_hw}"
    );
}

// =============================================================================
// Child side
// =============================================================================

#[test]
fn process_three_node_bus_child() {
    if std::env::var_os("TENTAFLOW_BUS_CHAOS_CHILD").is_none() {
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "info,tentaflow_core::bus=debug,tentaflow_core::sync=debug",
        ))
        .try_init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("child tokio runtime");
    runtime.block_on(child_main());
}

async fn child_main() {
    let home = PathBuf::from(std::env::var("TENTAFLOW_BUS_CHAOS_HOME").expect("child home env"));
    std::fs::create_dir_all(home.join("data")).expect("home data");
    unsafe {
        std::env::set_var("TENTAFLOW_HOME", &home);
        std::env::set_var("HOME", &home);
    }

    let db = tentaflow_core::db::init(&home.join("data").join("tentaflow.db")).expect("db");
    let cipher = Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0x53; 32]));
    let security = Arc::new(MeshSecurity::new(db.clone(), cipher.clone()).expect("security"));

    // Z12 harness knob. This MUST happen before `sync::runtime::init`, for two
    // independent reasons that both bite:
    //   * `replication::init` reads `services::environment::get_node_environment`
    //     once at call time (mirrors `tentaflow/src/main.rs`'s own read); and
    //   * `sync::runtime::init` is the ONLY place that resyncs the ledger's
    //     environment cache from the settings row
    //     (`runtime.rs`, end of `init`: "Without this, a settings row written
    //     outside `switch_node_environment` … would leave admission/outbox
    //     decisions serving a stale cached environment forever"). The cache is
    //     what stamps the `environment` field of every OUTGOING op
    //     (`runtime.rs:3084` -> `FjallSyncLedgerStore::current_environment`,
    //     `fjall_store.rs:1005`), and it defaults to `Prod` when the Fjall meta
    //     key is absent. Setting the knob after `init` therefore left this
    //     node's own ops stamped `Prod` while its payload said `test` — which
    //     is how a `test` node's topic row got past a `prod` node's envelope
    //     admission and made that node fence its own publish (measured 30.08
    //     18:47; agent G2 traced the mechanism, this line is the fix).
    // Deliberately NOT `switch_node_environment`: its `perform_environment_change`
    // wipes and reseeds core state, and that reseed does not cover
    // `BusTopic`/`BusPartitionAssignment` (G2's known deferred item), so it
    // would eat this node's own bus metadata instead of declaring an env.
    let local_env = match std::env::var("TENTAFLOW_BUS_CHAOS_ENV").ok().as_deref() {
        Some("test") => NodeEnvironment::Test,
        Some("dev") => NodeEnvironment::Dev,
        _ => NodeEnvironment::Prod,
    };
    tentaflow_core::services::environment::set_node_environment(&db, local_env)
        .expect("set node environment");

    let _runtime =
        tentaflow_core::sync::runtime::init(db.clone(), security.clone(), cipher.clone())
            .expect("runtime");
    let local_node_id = security.ed25519_public_key_hex();

    let mesh = IrohMeshManager::new(
        IrohMeshConfig {
            node_id: String::new(),
            bind_addr: "127.0.0.1:0".parse().expect("bind"),
            relay_url: None,
            enable_lan_discovery: false,
            enable_dht_discovery: false,
            addr_filter: None,
            disable_portmapper: false,
        },
        security.clone(),
    )
    .await
    .expect("mesh");
    let _mesh_task = mesh.start();

    // Generic ledger sync event plumbing — mirrors `tests/
    // process_four_node_sync.rs`'s `child_main` (Push/Ack/Pull/PullResponse/
    // SnapshotPull/SnapshotResponse), trimmed to only what a bus-replication
    // chaos test needs (no storage-proxy/services-registry/robot dispatch).
    let mut events = mesh.subscribe();
    let mesh_for_events = mesh.clone();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(IrohMeshEvent::SyncPushReceived { from_node_id, data }) => {
                    let Ok(payload) = tentaflow_protocol::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncPushPayload,
                    >(&data) else {
                        continue;
                    };
                    match tentaflow_core::sync::runtime::handle_push_payload(&from_node_id, payload)
                    {
                        Ok(Some(ack)) => {
                            if let Ok(bytes) = tentaflow_protocol::cbor::encode(&ack) {
                                let _ = mesh_for_events.send_sync_ack(&from_node_id, &bytes).await;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => eprintln!("bus-chaos child sync push error: {e}"),
                    }
                }
                Ok(IrohMeshEvent::SyncAckReceived { from_node_id, data }) => {
                    if let Ok(payload) = tentaflow_protocol::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncAckPayload,
                    >(&data)
                    {
                        let _ = tentaflow_core::sync::runtime::handle_ack_payload(
                            &from_node_id,
                            payload,
                        );
                    }
                }
                Ok(IrohMeshEvent::SyncPullReceived { from_node_id, data }) => {
                    let Ok(payload) = tentaflow_protocol::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncPullPayload,
                    >(&data) else {
                        continue;
                    };
                    let Ok(Some(result)) =
                        tentaflow_core::sync::runtime::handle_pull_payload(&from_node_id, payload)
                    else {
                        continue;
                    };
                    let _ = match result {
                        tentaflow_core::sync::runtime::MeshSyncPullResult::Operations(response) => {
                            match tentaflow_protocol::cbor::encode(&response) {
                                Ok(bytes) => {
                                    mesh_for_events
                                        .send_sync_pull_response(&from_node_id, &bytes)
                                        .await
                                }
                                Err(_) => Ok(()),
                            }
                        }
                        tentaflow_core::sync::runtime::MeshSyncPullResult::Snapshot(response) => {
                            match tentaflow_protocol::cbor::encode(&response) {
                                Ok(bytes) => {
                                    mesh_for_events
                                        .send_sync_snapshot_response(&from_node_id, &bytes)
                                        .await
                                }
                                Err(_) => Ok(()),
                            }
                        }
                    };
                }
                Ok(IrohMeshEvent::SyncPullResponseReceived { from_node_id, data }) => {
                    let Ok(payload) = tentaflow_protocol::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncPullResponsePayload,
                    >(&data) else {
                        continue;
                    };
                    if let Ok(Some(ack)) =
                        tentaflow_core::sync::runtime::handle_pull_response_payload(
                            &from_node_id,
                            payload,
                        )
                    {
                        if let Ok(bytes) = tentaflow_protocol::cbor::encode(&ack) {
                            let _ = mesh_for_events.send_sync_ack(&from_node_id, &bytes).await;
                        }
                    }
                }
                Ok(IrohMeshEvent::SyncSnapshotPullReceived { from_node_id, data }) => {
                    let Ok(payload) = tentaflow_protocol::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncSnapshotPullPayload,
                    >(&data) else {
                        continue;
                    };
                    if let Ok(Some(response)) =
                        tentaflow_core::sync::runtime::handle_snapshot_pull_payload(
                            &from_node_id,
                            payload,
                        )
                    {
                        if let Ok(bytes) = tentaflow_protocol::cbor::encode(&response) {
                            let _ = mesh_for_events
                                .send_sync_snapshot_response(&from_node_id, &bytes)
                                .await;
                        }
                    }
                }
                Ok(IrohMeshEvent::SyncSnapshotResponseReceived { from_node_id, data }) => {
                    let Ok(payload) = tentaflow_protocol::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncSnapshotResponsePayload,
                    >(&data) else {
                        continue;
                    };
                    if let Ok(Some(ack)) =
                        tentaflow_core::sync::runtime::handle_snapshot_response_payload(
                            &from_node_id,
                            payload,
                        )
                    {
                        if let Ok(bytes) = tentaflow_protocol::cbor::encode(&ack) {
                            let _ = mesh_for_events.send_sync_ack(&from_node_id, &bytes).await;
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    // TentaBus M1/M2 service + replication — mirrors `tentaflow/src/
    // main.rs`'s wiring order (bus first with an allow-all authorizer
    // instead of `RbacBusAuthorizer`, then replication once the mesh
    // manager exists).
    let bus_dir = home.join("bus");
    let svc = bus::init(BusInitConfig {
        bus_dir,
        db: db.clone(),
        authorizer: Arc::new(AllowAllAuthorizer),
        retention_interval: None,
        dedup_expected_rate_per_sec: 10_000,
        publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
        partition_handle_lru: None,
    })
    .expect("bus init");

    let provider: Arc<dyn tentaflow_core::bus::replication::glue::PartitionProvider> = svc.clone();
    let repl_cfg = ReplicationInitConfig {
        db: db.clone(),
        mesh: mesh.clone(),
        local_node_id: local_node_id.clone(),
        local_env,
        provider,
        lease_check_interval: ReplicationInitConfig::DEFAULT_LEASE_CHECK_INTERVAL,
    };
    let manager = replication_init(repl_cfg).await.expect("replication init");

    let assignment_store = SqliteLedgerAssignmentStore::new(db.clone());

    // Background auto-sync (module doc): push-if-pending, fall back to a
    // repair pull, then drain the inbox — every 150 ms, for every peer
    // this node has been told (via `TRUST`) to expect ledger traffic from.
    let trusted_peers: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let mesh = mesh.clone();
        let trusted_peers = trusted_peers.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(150));
            loop {
                ticker.tick().await;
                let peers = trusted_peers.lock().unwrap().clone();
                for peer in &peers {
                    match tentaflow_core::sync::runtime::build_push_payload_for_target(peer, 256) {
                        Ok(Some(payload)) => {
                            let op_count = payload.operations.len();
                            match tentaflow_protocol::cbor::encode(&payload) {
                                Ok(bytes) => {
                                    if let Err(e) = mesh.send_sync_push(peer, &bytes).await {
                                        eprintln!("TF3BUS auto-sync: send_sync_push to {peer} failed ({op_count} ops): {e}");
                                    }
                                }
                                Err(e) => {
                                    eprintln!("TF3BUS auto-sync: encode push to {peer} failed: {e}")
                                }
                            }
                        }
                        Ok(None) => {
                            match tentaflow_core::sync::runtime::build_repair_pull_payloads_for_peer(
                                peer, 16, 256,
                            ) {
                                Ok(payloads) => {
                                    for payload in payloads {
                                        if let Ok(bytes) =
                                            tentaflow_protocol::cbor::encode(&payload)
                                        {
                                            if let Err(e) = mesh.send_sync_pull(peer, &bytes).await
                                            {
                                                eprintln!("TF3BUS auto-sync: send_sync_pull to {peer} failed: {e}");
                                            }
                                        }
                                    }
                                }
                                Err(e) => eprintln!(
                                    "TF3BUS auto-sync: repair pull build for {peer} failed: {e}"
                                ),
                            }
                        }
                        Err(e) => eprintln!(
                            "TF3BUS auto-sync: build_push_payload_for_target({peer}) failed: {e}"
                        ),
                    }
                }
                if let Err(e) = tentaflow_core::sync::runtime::apply_unapplied_inbox(256) {
                    eprintln!("TF3BUS auto-sync: apply_unapplied_inbox failed: {e}");
                }
            }
        });
    }

    let addr = mesh
        .endpoint()
        .bound_sockets()
        .into_iter()
        .find(|addr| addr.is_ipv4())
        .expect("ipv4 bound socket");
    println!(
        "TF3BUS READY {} {} {}",
        local_node_id,
        security.public_key_hex(),
        addr
    );
    std::io::stdout().flush().expect("ready flush");

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.expect("stdin line");
        if line == "STOP" {
            mesh.shutdown().await;
            println!("TF3BUS OK STOP");
            break;
        }
        match handle_child_command(
            &line,
            &db,
            &cipher,
            &security,
            &mesh,
            &svc,
            &assignment_store,
            &manager,
            &trusted_peers,
        )
        .await
        {
            Ok(response) => println!("TF3BUS OK {response}"),
            Err(error) => println!("TF3BUS ERR {error}"),
        }
        std::io::stdout().flush().expect("command flush");
    }
}

async fn handle_child_command(
    line: &str,
    db: &tentaflow_core::db::DbPool,
    cipher: &Arc<tentaflow_core::crypto::SettingsCipher>,
    security: &MeshSecurity,
    mesh: &IrohMeshManager,
    svc: &Arc<tentaflow_core::bus::BusService>,
    assignment_store: &SqliteLedgerAssignmentStore,
    _manager: &Arc<tentaflow_core::bus::replication::manager::ReplicationManager>,
    trusted_peers: &Arc<Mutex<Vec<String>>>,
) -> anyhow::Result<String> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        ["TRUST", node_id, public_key] => {
            security.add_trusted_key(node_id, public_key, "bus-chaos-e2e", None)?;
            trusted_peers.lock().unwrap().push(node_id.to_string());
            // `MeshSecurity::add_trusted_key` only populates `trusted_nodes`
            // (the MESH-level trust table `is_trusted`/env-fencing reads).
            // Ledger sync targeting reads a SEPARATE table (`sync_nodes`,
            // `repository::list_sync_targets_for_resource` ->
            // `list_permission_filtered_sync_targets_with_conn`) plus a
            // `sync_policies` row for this org/addon — without both, `core.
            // bus_partition_assignment`/`core.bus_topic` ops never queue any
            // outbox targets at all, REGARDLESS of `CoreSyncScope::
            // Organization` (found the hard way: `queue_core_targets` ->
            // `list_sync_targets_for_resource` returns empty without an
            // enabled policy row, mirroring `tests/process_four_node_sync.
            // rs`'s own `SEED_SOURCE`/`SEED_RECEIVER` commands). `sync_profile
            // = "standard"` and `mode = "replicated_by_permission"` are
            // exactly what makes `is_default_core_sync_resource`'s blanket
            // "Organization + Durable -> every trusted sync_node" rule fire
            // for both bus resource kinds, without a per-resource ACL grant.
            tentaflow_core::db::repository::upsert_sync_node_identity(
                db, node_id, public_key, "ed25519", node_id, "server", "trusted", None, "standard",
            )?;
            tentaflow_core::db::repository::upsert_sync_policy(
                db,
                "bus-chaos-core-sync",
                ORG_ID,
                tentaflow_core::sync::core_registry::CORE_SYNC_ADDON_ID,
                None,
                None,
                "replicated_by_permission",
                None,
                None,
                true,
            )?;
            Ok("TRUST".to_string())
        }
        ["CONNECT", node_id, public_key, addr] => {
            let socket_addr = addr.parse::<SocketAddr>()?;
            mesh.connect_to_peer_direct(node_id, socket_addr).await?;
            wait_connected(mesh, node_id).await?;
            // `mesh.connect_to_peer_direct` establishes the GENERAL mesh
            // QUIC connection (used for ledger sync push/ack/pull) but does
            // NOT persist an address hint for `IrohMeshManager::connect_bus`
            // (M2's SEPARATE ALPN_BUS dial path, `net/iroh/pairing.rs`'s
            // `load_trusted_contact_hints`) — without this, a leader's
            // `Transport::open_stream(peer)` fails with "brak hintow dla
            // {peer}" even though the general mesh connection is up and
            // `is_trusted` passes. Production populates this table via the
            // real pairing PIN flow (`MeshSecurity::confirm_pairing` writes
            // both); this harness pairs via `TRUST`/`CONNECT` instead, so it
            // stores the hint directly.
            tentaflow_core::net::iroh::pairing::store_trusted_contact_hints(
                db,
                node_id,
                &tentaflow_core::net::iroh::pairing::PairingContactHints {
                    node_id: node_id.to_string(),
                    public_key_hex: public_key.to_string(),
                    hostname: String::new(),
                    addresses: vec![addr.to_string()],
                    relay_url: String::new(),
                },
            )?;
            Ok("CONNECT".to_string())
        }
        ["SET_PEER_ENV", node_id, env] => {
            let env = NodeEnvironment::parse(env)
                .ok_or_else(|| anyhow::anyhow!("unknown environment: {env}"))?;
            tentaflow_core::db::repository::set_trusted_node_environment(db, node_id, env)?;
            Ok("SET_PEER_ENV".to_string())
        }
        ["CREATE_TOPIC", org, topic, partitions, rf, acks, durability] => {
            let ctx = BusCallContext {
                org_id: org.to_string(),
                actor: Some("chaos-harness".to_string()),
                correlation_id: None,
                origin: "process_three_node_bus_failover".to_string(),
            };
            let opts = TopicOptions {
                partitions: Some(partitions.parse()?),
                replication_factor: Some(rf.parse()?),
                acks: Some(Acks::parse(acks).ok_or_else(|| anyhow::anyhow!("bad acks: {acks}"))?),
                durability_class: Some(
                    DurabilityClass::parse(durability)
                        .ok_or_else(|| anyhow::anyhow!("bad durability: {durability}"))?,
                ),
                ..Default::default()
            };
            match svc.create_topic(&ctx, topic, opts) {
                Ok(_) => Ok("CREATE_TOPIC".to_string()),
                Err(tentaflow_core::bus::BusServiceError::TopicAlreadyExists { .. }) => {
                    Ok("CREATE_TOPIC exists".to_string())
                }
                Err(e) => Err(anyhow::anyhow!("create_topic: {e}")),
            }
        }
        ["ASSIGN", org, topic, partition, leader_node_id, replica_csv] => {
            let replicas: Vec<String> = replica_csv.split(',').map(|s| s.to_string()).collect();
            let assignment = PartitionAssignment {
                instance_id: tentaflow_core::bus::instance::LEGACY_SINGLE_INSTANCE.to_string(),
                org_id: org.to_string(),
                topic: topic.to_string(),
                partition: partition.parse()?,
                leader_node_id: leader_node_id.to_string(),
                isr: replicas.clone(),
                replicas,
                leader_epoch: 1,
                updated_at_ms: now_ms(),
            };
            let op_id = assignment_store.propose(&assignment)?;
            // `propose` only appends to the ledger + queues this node's
            // OUTBOX for the OTHER replicas (`queue_core_targets` explicitly
            // skips `target.node_id == self.local_node_id` — a node is
            // never its own sync target). Nothing else applies a locally-
            // authored op back to THIS node's own materialized state: the
            // inbox/materializer path only runs for operations arriving FROM
            // another node — confirmed against `sync/runtime.rs`'s own test
            // helper `author_shared_secret`'s comment ("a real authoring
            // node also materializes its own write"), which calls
            // `core_materializer::apply_core_operation` by hand for exactly
            // this reason. Mirrored here: fetch the operation this node just
            // authored and materialize it locally the SAME way an inbound
            // push would, rather than calling `manager.apply_assignment`
            // directly — a direct call here would make the PROPOSER become
            // Leader (and start dialing followers) immediately, while the
            // other two replicas only discover the assignment up to 1s later
            // via their own poll loop, which was observed to lose the
            // resulting race: the leader's very first Hello to each follower
            // arrives before that follower's registry has the assignment at
            // all (`follower rejected Hello: TopicUnknown`), and the
            // supervisor's reconnect backoff (500 ms doubling to 5 s) can
            // then leave a follower stream down well past a single
            // `publish`'s ack-wait window. Self-materializing (not
            // self-applying) keeps entirely to the SAME 1 s poll-loop
            // cadence for all three nodes, so leader and followers converge
            // together instead of the leader racing ahead.
            let operation = tentaflow_core::sync::runtime::get_operation(op_id)?
                .ok_or_else(|| anyhow::anyhow!("just-authored operation not found in ledger"))?;
            tentaflow_core::sync::core_materializer::apply_core_operation(db, cipher, &operation)?;
            Ok("ASSIGN".to_string())
        }
        ["ROLE", org, topic, partition] => {
            let partition: u32 = partition.parse()?;
            let role = svc
                .replication()
                .map(|c| c.role(org, topic, partition))
                .ok_or_else(|| anyhow::anyhow!("no coordinator installed"))?;
            Ok(format!("ROLE {role:?}"))
        }
        // Live in-sync replica set as the LEADER sees it. `ROLE` above only
        // proves a node materialized the assignment locally; membership in
        // the leader's ISR is a later, separate fact (the leader dials each
        // replica and `register_follower` on an accepted Hello moves it in),
        // and `preflight`'s `NotEnoughReplicas` gate reads exactly this set.
        ["ISR", org, topic, partition] => {
            let partition: u32 = partition.parse()?;
            let coordinator = svc
                .replication()
                .ok_or_else(|| anyhow::anyhow!("no coordinator installed"))?;
            match coordinator
                .snapshot(org, Some(topic))
                .partitions
                .into_iter()
                .find(|p| p.topic == *topic && p.partition == partition)
            {
                Some(p) if !p.isr.is_empty() => {
                    Ok(format!("ISR {} {}", p.isr.len(), p.isr.join(",")))
                }
                Some(_) => Ok("ISR 0".to_string()),
                None => Ok("ISR 0".to_string()),
            }
        }
        ["PUBLISH_BATCH", org, topic, n_records, record_bytes] => {
            let n_records: usize = n_records.parse()?;
            let record_bytes: usize = record_bytes.parse()?;
            let ctx = BusCallContext {
                org_id: org.to_string(),
                actor: Some("chaos-harness".to_string()),
                correlation_id: None,
                origin: "process_three_node_bus_failover".to_string(),
            };
            let records = (0..n_records)
                .map(|_| PublishRecord {
                    key: None,
                    headers: Vec::new(),
                    payload: Bytes::from(vec![0xABu8; record_bytes]),
                    timestamp_ms: now_ms(),
                    schema_id: 0,
                })
                .collect();
            let batch = PublishBatch {
                partition: Some(PARTITION),
                producer: None,
                records,
            };
            let started = std::time::Instant::now();
            // `publish` (sync) blocks the calling thread on the partition
            // writer's response (`Partition::append_batch`'s
            // `resp_rx.blocking_recv()`) — fatal when called from a Tokio
            // worker thread ("Cannot block the current thread from within a
            // runtime"), since this handler runs inside the child's async
            // stdin command loop. `publish_async` is the documented async
            // twin for exactly this caller shape.
            let result = svc.publish_async(&ctx, topic, batch).await?;
            let elapsed_us = started.elapsed().as_micros();
            let ack = result
                .partitions
                .first()
                .ok_or_else(|| anyhow::anyhow!("publish returned no partition ack"))?;
            let stats = svc.partition_stats(&ctx, topic, ack.partition)?;
            Ok(format!(
                "PUBLISH_BATCH {} {} {} {}",
                ack.base_offset, ack.accepted, stats.high_watermark, elapsed_us
            ))
        }
        ["STATS", org, topic, partition] => {
            let ctx = BusCallContext {
                org_id: org.to_string(),
                actor: Some("chaos-harness".to_string()),
                correlation_id: None,
                origin: "process_three_node_bus_failover".to_string(),
            };
            let stats = svc.partition_stats(&ctx, topic, partition.parse()?)?;
            Ok(format!(
                "STATS {} {} {}",
                stats.earliest_offset, stats.high_watermark, stats.log_end_offset
            ))
        }
        ["HASH_LOG", org, topic, partition, upto_offset] => {
            let partition: u32 = partition.parse()?;
            let upto_offset: u64 = upto_offset.parse()?;
            let part = svc
                .partition(org, topic, partition)
                .map_err(|e| anyhow::anyhow!("partition: {e:?}"))?;
            let reader = part.open_reader();
            let mut hasher = Sha256::new();
            let mut count: u64 = 0;
            let mut cursor: u64 = 0;
            'outer: loop {
                if cursor >= upto_offset {
                    break;
                }
                let batches = reader
                    .fetch_from_offset(cursor, 4 * 1024 * 1024)
                    .map_err(|e| anyhow::anyhow!("fetch_from_offset: {e}"))?;
                if batches.is_empty() {
                    break;
                }
                let mut advanced = false;
                for view in &batches {
                    for rv in view.records_from(cursor) {
                        let rv = rv.map_err(|e| anyhow::anyhow!("decode record: {e}"))?;
                        let abs_offset = view.header().base_offset + rv.offset_delta as u64;
                        if abs_offset >= upto_offset {
                            break 'outer;
                        }
                        hasher.update(abs_offset.to_le_bytes());
                        if let Some(k) = &rv.key {
                            hasher.update(k);
                        }
                        hasher.update(&rv.payload);
                        count += 1;
                        cursor = abs_offset + 1;
                        advanced = true;
                    }
                }
                if !advanced {
                    break;
                }
            }
            let digest = hasher.finalize();
            Ok(format!("HASH_LOG {} {count}", hex_encode(&digest)))
        }
        _ => anyhow::bail!("unknown command: {line}"),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

async fn wait_connected(mesh: &IrohMeshManager, node_id: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if mesh.is_connected(node_id).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!("mesh peer not connected: {node_id}")
}
