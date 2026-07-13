// =============================================================================
// Plik: tests/process_four_node_sync.rs
// Opis: Procesowy test E2E synchronizacji 4 lokalnych nodow. Parent odpala
//       cztery child-procesy z osobnymi home/db/ledger i steruje nimi po stdin.
// Przykład: cargo test --test process_four_node_sync
//           process_four_node_sync_fanout_survives_restart
// =============================================================================

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use rusqlite::OptionalExtension;
use serde_json::Value as JsonValue;
use tentaflow_core::db::repository;
use tentaflow_core::mesh::iroh_manager::{IrohMeshConfig, IrohMeshEvent, IrohMeshManager};
use tentaflow_core::mesh::security::MeshSecurity;
use tentaflow_core::sync::core_capture::CoreWriteCapture;
use tentaflow_core::sync::core_registry::{
    descriptor_for_kind, CoreSyncResourceKind, CORE_SYNC_ADDON_ID,
};
use tentaflow_core::sync::ledger::{FieldValue, OperationId};
use tentaflow_core::sync::runtime::{MeshSyncPullResult, SqlWriteAction, SqlWriteCapture};
use tentaflow_protocol::mesh::{
    StorageProxyRequestKind, StorageProxyRequestPayload, StorageValueWire,
};

const FLOW_ID: &str = "92001";
const SUITE_ORG_ID: &str = "org-process";
const SUITE_USER_ID: &str = "20101";
const SUITE_GROUP_ID: &str = "20102";
const SUITE_FLOW_ID: &str = "20103";
const SUITE_BINDING_ID: &str = "20104";
const SUITE_ROLE_ID: &str = "process-sync-role";
const SUITE_MODEL_PATTERN: &str = "process-sync-model";
const SNAPSHOT_ADDON_ID: &str = "process-snapshot-addon";
const SNAPSHOT_PARTITION: &str = "addon/process-snapshot-addon/person/person-1";

struct ChildNode {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    node_id: String,
    public_key: String,
    addr: SocketAddr,
}

impl ChildNode {
    fn spawn(name: &str, home: PathBuf) -> Self {
        let exe = std::env::current_exe().expect("current test exe");
        let mut child = Command::new(exe)
            .arg("--exact")
            .arg("process_four_node_child")
            .arg("--nocapture")
            .env("TENTAFLOW_PROCESS_E2E_CHILD", "1")
            .env("TENTAFLOW_PROCESS_E2E_HOME", home)
            .env("TENTAFLOW_PROCESS_E2E_NAME", name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn child node");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        let mut node = Self {
            child,
            stdin,
            stdout,
            node_id: String::new(),
            public_key: String::new(),
            addr: "127.0.0.1:0".parse().expect("addr"),
        };
        let ready = node.read_prefixed_line("READY", Duration::from_secs(20));
        let parts = ready.split_whitespace().collect::<Vec<_>>();
        assert_eq!(
            parts.len(),
            5,
            "READY line must include node_id public_key addr"
        );
        node.node_id = parts[2].to_string();
        node.public_key = parts[3].to_string();
        node.addr = parts[4].parse().expect("ready addr");
        node
    }

    fn command(&mut self, command: &str) -> String {
        eprintln!("TF4 parent -> {}: {command}", self.node_id);
        writeln!(self.stdin, "{command}").expect("write child command");
        self.stdin.flush().expect("flush child command");
        self.read_command_response(command, Duration::from_secs(30))
    }

    fn read_prefixed_line(&mut self, prefix: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let needle = format!("TF4 {prefix}");
        let mut line = String::new();
        loop {
            assert!(Instant::now() <= deadline, "timeout waiting for {prefix}");
            line.clear();
            let read = self.stdout.read_line(&mut line).expect("read child line");
            assert_ne!(read, 0, "child exited before {prefix}");
            let trimmed = line.trim();
            if trimmed.starts_with(&needle) {
                return trimmed.to_string();
            }
        }
    }

    fn read_command_response(&mut self, command: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let mut line = String::new();
        loop {
            assert!(
                Instant::now() <= deadline,
                "timeout waiting for command response: {command}"
            );
            line.clear();
            let read = self.stdout.read_line(&mut line).expect("read child line");
            assert_ne!(read, 0, "child exited during command: {command}");
            let trimmed = line.trim();
            if trimmed.starts_with("TF4 OK") {
                eprintln!("TF4 parent <- {}: {trimmed}", self.node_id);
                return trimmed.to_string();
            }
            if trimmed.starts_with("TF4 ERR") {
                panic!("child command failed: {command}: {trimmed}");
            }
        }
    }
}

impl Drop for ChildNode {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "STOP");
        let _ = self.stdin.flush();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn process_four_node_child() {
    if std::env::var_os("TENTAFLOW_PROCESS_E2E_CHILD").is_none() {
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("child tokio runtime");
    runtime.block_on(child_main());
}

#[test]
fn process_four_node_sync_fanout_survives_restart() {
    if std::env::var_os("TENTAFLOW_PROCESS_E2E_CHILD").is_some() {
        return;
    }

    let root = tempfile::tempdir().expect("process e2e root");
    let mut source = ChildNode::spawn("source", root.path().join("source"));
    let mut receivers = vec![
        ChildNode::spawn("receiver-a", root.path().join("receiver-a")),
        ChildNode::spawn("receiver-b", root.path().join("receiver-b")),
        ChildNode::spawn("receiver-c", root.path().join("receiver-c")),
    ];

    for receiver in &mut receivers {
        connect_nodes(&mut source, receiver);
    }

    let seed_source_args = receivers
        .iter()
        .map(|receiver| format!("{} {}", receiver.node_id, receiver.public_key))
        .collect::<Vec<_>>()
        .join(" ");
    source.command(&format!(
        "SEED_SOURCE {} {}",
        receivers.len(),
        seed_source_args
    ));
    for receiver in &mut receivers {
        receiver.command("SEED_RECEIVER");
    }

    let op_line = source.command("RECORD_FLOW");
    let op_id = parse_record_flow_op_id(&op_line);
    for receiver in &mut receivers {
        source.command(&format!("PUSH {}", receiver.node_id));
    }
    source.command(&format!("WAIT_ACKS {} {}", op_id, receivers.len()));

    for receiver in &mut receivers {
        receiver.command("WAIT_FLOW");
    }

    drop(source);
    drop(receivers);

    let mut source = ChildNode::spawn("source-restart", root.path().join("source"));
    let mut receivers = vec![
        ChildNode::spawn("receiver-a-restart", root.path().join("receiver-a")),
        ChildNode::spawn("receiver-b-restart", root.path().join("receiver-b")),
        ChildNode::spawn("receiver-c-restart", root.path().join("receiver-c")),
    ];
    for receiver in &mut receivers {
        receiver.command("WAIT_FLOW");
    }
    source.command(&format!("ASSERT_NO_PENDING {}", op_id));
}

#[test]
fn process_four_node_offline_receiver_catches_up_after_source_restart() {
    if std::env::var_os("TENTAFLOW_PROCESS_E2E_CHILD").is_some() {
        return;
    }

    let root = tempfile::tempdir().expect("process e2e root");
    let source_home = root.path().join("source");
    let receiver_a_home = root.path().join("receiver-a");
    let receiver_b_home = root.path().join("receiver-b");
    let receiver_c_home = root.path().join("receiver-c");

    let mut source = ChildNode::spawn("source", source_home.clone());
    let mut receiver_a = ChildNode::spawn("receiver-a", receiver_a_home);
    let mut receiver_b = ChildNode::spawn("receiver-b", receiver_b_home);
    let mut receiver_c = ChildNode::spawn("receiver-c", receiver_c_home.clone());

    connect_nodes(&mut source, &mut receiver_a);
    connect_nodes(&mut source, &mut receiver_b);
    connect_nodes(&mut source, &mut receiver_c);

    let seed_source_args = [&receiver_a, &receiver_b, &receiver_c]
        .iter()
        .map(|receiver| format!("{} {}", receiver.node_id, receiver.public_key))
        .collect::<Vec<_>>()
        .join(" ");
    source.command(&format!("SEED_SOURCE 3 {}", seed_source_args));
    receiver_a.command("SEED_RECEIVER");
    receiver_b.command("SEED_RECEIVER");
    receiver_c.command("SEED_RECEIVER");

    let offline_node_id = receiver_c.node_id.clone();
    drop(receiver_c);

    let op_line = source.command("RECORD_FLOW");
    let op_id = parse_record_flow_op_id(&op_line);
    source.command(&format!("PUSH {}", receiver_a.node_id));
    source.command(&format!("PUSH {}", receiver_b.node_id));
    source.command(&format!("WAIT_ACKS {} 2", op_id));
    receiver_a.command("WAIT_FLOW");
    receiver_b.command("WAIT_FLOW");

    drop(receiver_a);
    drop(receiver_b);
    drop(source);

    let mut source = ChildNode::spawn("source-restart", source_home);
    let mut receiver_c = ChildNode::spawn("receiver-c-restart", receiver_c_home);
    assert_eq!(receiver_c.node_id, offline_node_id);

    connect_nodes(&mut source, &mut receiver_c);
    source.command(&format!("PUSH {}", receiver_c.node_id));
    source.command(&format!("WAIT_ACKS {} 3", op_id));
    receiver_c.command("WAIT_FLOW");
}

#[test]
fn process_four_node_permission_gating_blocks_unshared_target() {
    if std::env::var_os("TENTAFLOW_PROCESS_E2E_CHILD").is_some() {
        return;
    }

    let root = tempfile::tempdir().expect("process e2e root");
    let mut source = ChildNode::spawn("source", root.path().join("source"));
    let mut receiver_a = ChildNode::spawn("receiver-a", root.path().join("receiver-a"));
    let mut receiver_b = ChildNode::spawn("receiver-b", root.path().join("receiver-b"));
    let mut receiver_c = ChildNode::spawn("receiver-c", root.path().join("receiver-c"));

    connect_nodes(&mut source, &mut receiver_a);
    connect_nodes(&mut source, &mut receiver_b);
    connect_nodes(&mut source, &mut receiver_c);

    let seed_source_args = [&receiver_a, &receiver_b, &receiver_c]
        .iter()
        .map(|receiver| format!("{} {}", receiver.node_id, receiver.public_key))
        .collect::<Vec<_>>()
        .join(" ");
    source.command(&format!("SEED_SOURCE_ALLOWED 3 2 {}", seed_source_args));
    receiver_a.command("SEED_RECEIVER");
    receiver_b.command("SEED_RECEIVER");
    receiver_c.command("SEED_RECEIVER");

    let op_line = source.command("RECORD_FLOW");
    let op_id = parse_record_flow_op_id(&op_line);
    source.command(&format!("ASSERT_NO_PAYLOAD {}", receiver_c.node_id));
    source.command(&format!("PUSH {}", receiver_a.node_id));
    source.command(&format!("PUSH {}", receiver_b.node_id));
    source.command(&format!("WAIT_ACKS {} 2", op_id));
    receiver_a.command("WAIT_FLOW");
    receiver_b.command("WAIT_FLOW");
    receiver_c.command("ASSERT_NO_FLOW");

    // Grant receiver_c access to the ALREADY-minted flow and let the authority's
    // backfill re-enqueue it — no new mint, no manual payload. This is the active
    // upgrade trigger: without the backfill the op never enters receiver_c's outbox
    // and it would never materialize. BACKFILL_GRANTS runs the same code the sync
    // repair scheduler tick runs in production.
    source.command(&format!("GRANT_SOURCE_TARGET {}", receiver_c.node_id));
    let backfilled = source.command("BACKFILL_GRANTS");
    assert!(
        backfilled.ends_with(" 1") || backfilled.contains(" 1"),
        "grant must re-enqueue exactly the previously-redacted op, got: {backfilled}"
    );
    source.command(&format!("PUSH {}", receiver_c.node_id));
    source.command(&format!("WAIT_ACKS {} 3", op_id));
    receiver_c.command("WAIT_FLOW");

    // A second backfill is a no-op (epoch watermark already current), so re-running
    // the scheduler tick does not duplicate outbox entries.
    let again = source.command("BACKFILL_GRANTS");
    assert!(
        again.ends_with(" 0"),
        "backfill is idempotent once the epoch is recorded, got: {again}"
    );
}

#[test]
fn process_four_node_central_only_clients_do_not_materialize_sql() {
    if std::env::var_os("TENTAFLOW_PROCESS_E2E_CHILD").is_some() {
        return;
    }

    let root = tempfile::tempdir().expect("process e2e root");
    let mut authority = ChildNode::spawn("authority", root.path().join("authority"));
    let mut replicated = ChildNode::spawn("replicated", root.path().join("replicated"));
    let mut central_a = ChildNode::spawn("central-a", root.path().join("central-a"));
    let mut central_b = ChildNode::spawn("central-b", root.path().join("central-b"));

    connect_nodes(&mut authority, &mut replicated);
    connect_nodes(&mut authority, &mut central_a);
    connect_nodes(&mut authority, &mut central_b);

    let seed_source_args = [&replicated, &central_a, &central_b]
        .iter()
        .map(|receiver| format!("{} {}", receiver.node_id, receiver.public_key))
        .collect::<Vec<_>>()
        .join(" ");
    authority.command(&format!("SEED_SQL_SOURCE 3 1 {}", seed_source_args));
    replicated.command("SEED_SQL_RECEIVER");
    central_a.command("SEED_SQL_RECEIVER");
    central_b.command("SEED_SQL_RECEIVER");

    let sql_op = parse_record_flow_op_id(&authority.command("RECORD_SQL_INSERT central-source"));
    let snapshot_line = authority.command("BUILD_SQL_SNAPSHOT 1");
    let snapshot = parse_snapshot_line(&snapshot_line);

    authority.command(&format!("ASSERT_NO_PAYLOAD {}", central_a.node_id));
    authority.command(&format!("ASSERT_NO_PAYLOAD {}", central_b.node_id));
    authority.command(&format!("ASSERT_REPAIR_REDACTED {}", central_a.node_id));
    authority.command(&format!("ASSERT_REPAIR_REDACTED {}", central_b.node_id));
    authority.command(&format!(
        "ASSERT_SNAPSHOT_DENIED {} {} {}",
        central_a.node_id, snapshot.0, snapshot.1
    ));
    authority.command(&format!(
        "ASSERT_SNAPSHOT_DENIED {} {} {}",
        central_b.node_id, snapshot.0, snapshot.1
    ));

    authority.command(&format!("PUSH {}", replicated.node_id));
    authority.command(&format!("WAIT_ACKS {} 1", sql_op));
    replicated.command("WAIT_SQL_NAME central-source");
    central_a.command("ASSERT_NO_SQL");
    central_b.command("ASSERT_NO_SQL");
}

// Authority-write through a central client labels the capture by its raw table
// (`contacts/<hash>`), not the replicated logical resource (`person/person-1`).
// Under per-node hash-chains that unsubscribed `contacts` op sits on the same
// authority chain, BEFORE the `person/person-1` op `replicated` does subscribe
// to. The serving side now answers a chain pull by REDACTING the op the requester
// is not a sync target for (signed chain proof, no body) instead of aborting, so
// `replicated` advances its node-frontier past the redacted position and still
// materializes the later op it is permitted to receive. This is the regression
// guard for the per-node chain serving deadlock.
#[test]
fn process_four_node_central_only_clients_read_and_write_through_authority() {
    if std::env::var_os("TENTAFLOW_PROCESS_E2E_CHILD").is_some() {
        return;
    }

    let root = tempfile::tempdir().expect("process e2e root");
    let mut authority = ChildNode::spawn("authority", root.path().join("authority"));
    let mut replicated = ChildNode::spawn("replicated", root.path().join("replicated"));
    let mut central_a = ChildNode::spawn("central-a", root.path().join("central-a"));
    let mut central_b = ChildNode::spawn("central-b", root.path().join("central-b"));

    connect_nodes(&mut authority, &mut replicated);
    connect_nodes(&mut authority, &mut central_a);
    connect_nodes(&mut authority, &mut central_b);

    let seed_source_args = [&replicated, &central_a, &central_b]
        .iter()
        .map(|receiver| format!("{} {}", receiver.node_id, receiver.public_key))
        .collect::<Vec<_>>()
        .join(" ");
    authority.command(&format!("SEED_SQL_SOURCE 3 1 {}", seed_source_args));
    replicated.command("SEED_SQL_RECEIVER");
    central_a.command(&format!(
        "SEED_SQL_CENTRAL_CLIENT {} {}",
        authority.node_id, authority.public_key
    ));
    central_b.command(&format!(
        "SEED_SQL_CENTRAL_CLIENT {} {}",
        authority.node_id, authority.public_key
    ));

    authority.command("LOCAL_SQL_INSERT central-source");
    central_a.command(&format!(
        "REMOTE_SQL_QUERY {} central-source",
        authority.node_id
    ));
    central_a.command(&format!(
        "REMOTE_SQL_EXEC {} central-write-a",
        authority.node_id
    ));
    authority.command("WAIT_SQL_NAME central-write-a");
    central_b.command(&format!(
        "REMOTE_SQL_QUERY {} central-write-a",
        authority.node_id
    ));

    let sql_op = parse_record_flow_op_id(&authority.command("RECORD_SQL_INSERT replicated-copy"));
    // The authority chain now holds, in node_seq order: the replicated person op
    // (seq 1, replicated IS a target), the central client's raw `contacts` write
    // (seq 2, replicated is NOT a target) and `replicated-copy` (seq 3, replicated
    // IS a target). The push carries only seqs 1 and 3, so seq 3 GAPS on seq 2 at
    // the receiver and queues a per-node repair. The repair pull is where the fix
    // bites: the authority serves seq 2 REDACTED (signed chain proof, no body) so
    // `replicated` advances past it and admits seq 3 — instead of the old hard
    // reject that stalled the chain forever.
    authority.command(&format!("PUSH {}", replicated.node_id));
    // The push carries only `replicated-copy`, which gaps on the unsubscribed
    // `contacts` op ahead of it and queues a per-node repair. SEND_REPAIR drives
    // that repair pull: the authority serves the unsubscribed op REDACTED so
    // `replicated` advances past it and admits `replicated-copy`. `send_repair_pull`
    // retries until the gap's repair request is queued, so there is no race.
    replicated.command(&format!("SEND_REPAIR {}", authority.node_id));
    authority.command(&format!("WAIT_ACKS {} 1", sql_op));
    replicated.command("WAIT_SQL_NAME replicated-copy");
    central_a.command("ASSERT_NO_SQL");
    central_b.command("ASSERT_NO_SQL");
}

#[test]
fn process_four_node_core_suite_materializes_after_restart() {
    if std::env::var_os("TENTAFLOW_PROCESS_E2E_CHILD").is_some() {
        return;
    }

    let root = tempfile::tempdir().expect("process e2e root");
    let source_home = root.path().join("source");
    let receiver_homes = [
        root.path().join("receiver-a"),
        root.path().join("receiver-b"),
        root.path().join("receiver-c"),
    ];
    let mut source = ChildNode::spawn("source", source_home.clone());
    let mut receivers = vec![
        ChildNode::spawn("receiver-a", receiver_homes[0].clone()),
        ChildNode::spawn("receiver-b", receiver_homes[1].clone()),
        ChildNode::spawn("receiver-c", receiver_homes[2].clone()),
    ];

    for receiver in &mut receivers {
        connect_nodes(&mut source, receiver);
    }

    let seed_source_args = receivers
        .iter()
        .map(|receiver| format!("{} {}", receiver.node_id, receiver.public_key))
        .collect::<Vec<_>>()
        .join(" ");
    source.command(&format!(
        "SEED_SOURCE_CORE_SUITE {} {}",
        receivers.len(),
        seed_source_args
    ));
    for receiver in &mut receivers {
        receiver.command("SEED_RECEIVER_CORE_SUITE");
    }

    let op_ids = parse_record_core_suite_op_ids(&source.command("RECORD_CORE_SUITE"));
    for receiver in &mut receivers {
        source.command(&format!("PUSH {}", receiver.node_id));
    }
    for op_id in &op_ids {
        source.command(&format!("WAIT_ACKS {} {}", op_id, receivers.len()));
    }
    for receiver in &mut receivers {
        receiver.command("WAIT_CORE_SUITE");
    }

    drop(receivers);
    drop(source);

    let mut source = ChildNode::spawn("source-restart", source_home);
    let mut receivers = vec![
        ChildNode::spawn("receiver-a-restart", receiver_homes[0].clone()),
        ChildNode::spawn("receiver-b-restart", receiver_homes[1].clone()),
        ChildNode::spawn("receiver-c-restart", receiver_homes[2].clone()),
    ];
    for receiver in &mut receivers {
        receiver.command("WAIT_CORE_SUITE");
    }
    for op_id in &op_ids {
        source.command(&format!("ASSERT_NO_PENDING {}", op_id));
    }
}

#[test]
fn process_four_node_snapshot_tail_respects_acl() {
    if std::env::var_os("TENTAFLOW_PROCESS_E2E_CHILD").is_some() {
        return;
    }

    let root = tempfile::tempdir().expect("process e2e root");
    let mut source = ChildNode::spawn("source", root.path().join("source"));
    let mut receiver_a = ChildNode::spawn("receiver-a", root.path().join("receiver-a"));
    let mut receiver_b = ChildNode::spawn("receiver-b", root.path().join("receiver-b"));
    let mut receiver_denied = ChildNode::spawn("receiver-denied", root.path().join("receiver-c"));

    connect_nodes(&mut source, &mut receiver_a);
    connect_nodes(&mut source, &mut receiver_b);
    connect_nodes(&mut source, &mut receiver_denied);

    let seed_source_args = [&receiver_a, &receiver_b, &receiver_denied]
        .iter()
        .map(|receiver| format!("{} {}", receiver.node_id, receiver.public_key))
        .collect::<Vec<_>>()
        .join(" ");
    source.command(&format!("SEED_SQL_SOURCE 3 2 {}", seed_source_args));
    receiver_a.command("SEED_SQL_RECEIVER");
    receiver_b.command("SEED_SQL_RECEIVER");
    receiver_denied.command("SEED_SQL_RECEIVER");

    source.command("RECORD_SQL_INSERT snap-base");
    let snapshot_line = source.command("BUILD_SQL_SNAPSHOT 1");
    let snapshot = parse_snapshot_line(&snapshot_line);
    let update_op = parse_record_flow_op_id(&source.command("RECORD_SQL_UPDATE snap-tail"));

    // A non-target chain pull is served REDACTED (no body leak); a snapshot pull
    // is still hard-denied because a SQL snapshot package cannot be partially
    // redacted (all-or-nothing per partition).
    source.command(&format!(
        "ASSERT_REPAIR_REDACTED {}",
        receiver_denied.node_id
    ));
    source.command(&format!(
        "ASSERT_SNAPSHOT_DENIED {} {} {}",
        receiver_denied.node_id, snapshot.0, snapshot.1
    ));
    receiver_a.command(&format!(
        "SEND_SNAPSHOT {} {} {}",
        source.node_id, snapshot.0, snapshot.1
    ));
    receiver_b.command(&format!(
        "SEND_SNAPSHOT {} {} {}",
        source.node_id, snapshot.0, snapshot.1
    ));
    receiver_a.command("WAIT_SQL_NAME snap-tail");
    receiver_b.command("WAIT_SQL_NAME snap-tail");
    source.command(&format!("WAIT_ACKS {} 2", update_op));
}

#[test]
fn process_four_node_conflict_and_repeated_fanout() {
    if std::env::var_os("TENTAFLOW_PROCESS_E2E_CHILD").is_some() {
        return;
    }

    let root = tempfile::tempdir().expect("process e2e root");
    let mut source = ChildNode::spawn("source", root.path().join("source"));
    let mut receiver_a = ChildNode::spawn("receiver-a", root.path().join("receiver-a"));
    let mut receiver_b = ChildNode::spawn("receiver-b", root.path().join("receiver-b"));
    let mut receiver_c = ChildNode::spawn("receiver-c", root.path().join("receiver-c"));

    connect_nodes(&mut source, &mut receiver_a);
    connect_nodes(&mut source, &mut receiver_b);
    connect_nodes(&mut source, &mut receiver_c);

    let seed_source_args = [&receiver_a, &receiver_b, &receiver_c]
        .iter()
        .map(|receiver| format!("{} {}", receiver.node_id, receiver.public_key))
        .collect::<Vec<_>>()
        .join(" ");
    source.command(&format!("SEED_SQL_SOURCE 3 3 {}", seed_source_args));
    receiver_a.command("SEED_SQL_RECEIVER");
    receiver_b.command("SEED_SQL_RECEIVER");
    receiver_c.command("SEED_SQL_RECEIVER");
    receiver_a.command("LOCAL_SQL_INSERT local-conflict");

    let sql_op = parse_record_flow_op_id(&source.command("RECORD_SQL_INSERT remote-conflict"));
    for receiver in [&receiver_a, &receiver_b, &receiver_c] {
        source.command(&format!("PUSH {}", receiver.node_id));
    }
    source.command(&format!("WAIT_ACKS {} 3", sql_op));
    receiver_a.command("WAIT_SQL_CONFLICT");
    receiver_b.command("WAIT_SQL_NAME remote-conflict");
    receiver_c.command("WAIT_SQL_NAME remote-conflict");

    source.command(&format!("SEED_SOURCE 3 {}", seed_source_args));
    receiver_a.command("SEED_RECEIVER");
    receiver_b.command("SEED_RECEIVER");
    receiver_c.command("SEED_RECEIVER");
    let mut last_op = String::new();
    for idx in 0..8 {
        last_op =
            parse_record_flow_op_id(&source.command(&format!("RECORD_FLOW_ID 92001 flow-{idx}")));
        for receiver in [&receiver_a, &receiver_b, &receiver_c] {
            source.command(&format!("PUSH {}", receiver.node_id));
        }
    }
    source.command(&format!("WAIT_ACKS {} 3", last_op));
    receiver_a.command("WAIT_FLOW_NAME flow-7");
    receiver_b.command("WAIT_FLOW_NAME flow-7");
    receiver_c.command("WAIT_FLOW_NAME flow-7");
}

// Direct on-the-wire proof that the per-node hash-chain redesign is
// conflict-free: all four nodes write the SAME `core.flow` resource concurrently
// (each with its own value and HLC) BEFORE any exchange, then fully exchange
// every chain. HLC-LWW must drive all four to byte-identical converged state
// with no deadlock, no repair spin, and no HashChainMismatch — the fork bug this
// redesign closes would otherwise leave nodes disagreeing on the winner.
#[test]
fn process_four_node_concurrent_write_converges() {
    if std::env::var_os("TENTAFLOW_PROCESS_E2E_CHILD").is_some() {
        return;
    }

    let root = tempfile::tempdir().expect("process e2e root");
    let mut nodes = vec![
        ChildNode::spawn("converge-a", root.path().join("converge-a")),
        ChildNode::spawn("converge-b", root.path().join("converge-b")),
        ChildNode::spawn("converge-c", root.path().join("converge-c")),
        ChildNode::spawn("converge-d", root.path().join("converge-d")),
    ];

    // Full-mesh trust + connect, then teach every node about the other three as
    // trusted sync peers so each fans out its own write to the rest.
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            let (left, right) = nodes.split_at_mut(j);
            connect_nodes(&mut left[i], &mut right[0]);
        }
    }
    let identities = nodes
        .iter()
        .map(|node| (node.node_id.clone(), node.public_key.clone()))
        .collect::<Vec<_>>();
    for (idx, node) in nodes.iter_mut().enumerate() {
        for (peer_idx, (peer_id, peer_key)) in identities.iter().enumerate() {
            if peer_idx != idx {
                node.command(&format!("SEED_PEER {peer_id} {peer_key}"));
            }
        }
    }

    // Concurrent writes: each node stamps the same resource with a distinct value
    // before exchanging anything, so the chains genuinely fork.
    let names = ["flow-from-a", "flow-from-b", "flow-from-c", "flow-from-d"];
    for (idx, node) in nodes.iter_mut().enumerate() {
        node.command(&format!("RECORD_FLOW_NAMED {}", names[idx]));
    }

    // Full exchange: every node pushes its single-op chain to every other node.
    // Push receipt is applied on the receiver's async event task, so re-push the
    // full fanout and re-read until all four agree (a pending outbox entry whose
    // target was mid-apply gets another delivery) or the deadline fails the test.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut converged;
    loop {
        for idx in 0..nodes.len() {
            for peer_idx in 0..identities.len() {
                if peer_idx != idx {
                    let peer_id = identities[peer_idx].0.clone();
                    nodes[idx].command(&format!("PUSH_IF_PENDING {peer_id}"));
                }
            }
        }
        // A push is acked at the protocol level even when the receiver's inbox
        // apply was deferred (ordering) — production drains the inbox on a timer,
        // so re-run the drain here to materialize any buffered op before reading.
        for node in nodes.iter_mut() {
            node.command("APPLY_INBOX");
        }
        std::thread::sleep(Duration::from_millis(200));
        converged = nodes
            .iter_mut()
            .map(|node| {
                let line = node.command("READ_FLOW_NAME");
                line.split_whitespace()
                    .nth(3)
                    .expect("converged flow name")
                    .to_string()
            })
            .collect::<Vec<_>>();
        let first = &converged[0];
        let all_agree = first != "<none>" && converged.iter().all(|value| value == first);
        if all_agree {
            break;
        }
        if Instant::now() > deadline {
            let hlcs = nodes
                .iter_mut()
                .map(|node| node.command("READ_FLOW_HLC"))
                .collect::<Vec<_>>();
            panic!("nodes did not converge within deadline: {converged:?}\nHLCs: {hlcs:#?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // The converged value must be one of the four concurrent writes — LWW picked a
    // real op, not a corrupted or empty merge.
    let winner = &converged[0];
    assert!(
        names.contains(&winner.as_str()),
        "converged value {winner} is not one of the concurrent writes {names:?}"
    );
}

fn connect_nodes(source: &mut ChildNode, receiver: &mut ChildNode) {
    source.command(&format!(
        "TRUST {} {}",
        receiver.node_id, receiver.public_key
    ));
    receiver.command(&format!("TRUST {} {}", source.node_id, source.public_key));
    source.command(&format!("CONNECT {} {}", receiver.node_id, receiver.addr));
    receiver.command(&format!("CONNECT {} {}", source.node_id, source.addr));
}

fn parse_record_flow_op_id(line: &str) -> String {
    line.split_whitespace()
        .nth(3)
        .expect("record flow op id")
        .to_string()
}

fn parse_record_core_suite_op_ids(line: &str) -> Vec<String> {
    line.split_whitespace()
        .skip(3)
        .map(ToString::to_string)
        .collect()
}

fn parse_snapshot_line(line: &str) -> (u64, String) {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    (
        parts
            .get(3)
            .expect("snapshot sequence")
            .parse()
            .expect("snapshot sequence number"),
        parts.get(4).expect("snapshot id").to_string(),
    )
}

async fn child_main() {
    let home = PathBuf::from(std::env::var("TENTAFLOW_PROCESS_E2E_HOME").expect("child home env"));
    std::fs::create_dir_all(home.join("data")).expect("home data");
    unsafe {
        std::env::set_var("TENTAFLOW_HOME", &home);
        std::env::set_var("HOME", &home);
    }

    let db = tentaflow_core::db::init(&home.join("data").join("tentaflow.db")).expect("db");
    let cipher = std::sync::Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0x44; 32]));
    let security =
        std::sync::Arc::new(MeshSecurity::new(db.clone(), cipher.clone()).expect("security"));
    let _runtime =
        tentaflow_core::sync::runtime::init(db.clone(), security.clone(), cipher).expect("runtime");
    let local_node_id = security.ed25519_public_key_hex();
    let mesh = std::sync::Arc::new(
        IrohMeshManager::new(
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
        .expect("mesh"),
    );
    let _mesh_task = mesh.start();
    let mut events = mesh.subscribe();
    let mesh_for_events = mesh.clone();
    let db_for_events = db.clone();
    let local_node_id_for_events = local_node_id.clone();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(IrohMeshEvent::SyncPushReceived { from_node_id, data }) => {
                    let payload = tentaflow_protocol::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncPushPayload,
                    >(&data)
                    .expect("decode sync push");
                    match tentaflow_core::sync::runtime::handle_push_payload(&from_node_id, payload)
                    {
                        Ok(Some(ack)) => {
                            let ack_bytes =
                                tentaflow_protocol::cbor::encode(&ack).expect("encode ack");
                            mesh_for_events
                                .send_sync_ack(&from_node_id, &ack_bytes)
                                .await
                                .expect("send ack");
                        }
                        Ok(None) => {}
                        Err(error) => eprintln!("TF4 child sync push error: {error}"),
                    }
                }
                Ok(IrohMeshEvent::SyncAckReceived { from_node_id, data }) => {
                    let payload = tentaflow_protocol::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncAckPayload,
                    >(&data)
                    .expect("decode sync ack");
                    tentaflow_core::sync::runtime::handle_ack_payload(&from_node_id, payload)
                        .expect("handle ack");
                }
                Ok(IrohMeshEvent::SyncPullReceived { from_node_id, data }) => {
                    let payload = tentaflow_protocol::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncPullPayload,
                    >(&data)
                    .expect("decode sync pull");
                    let Some(result) =
                        tentaflow_core::sync::runtime::handle_pull_payload(&from_node_id, payload)
                            .expect("handle pull")
                    else {
                        continue;
                    };
                    match result {
                        MeshSyncPullResult::Operations(response) => {
                            mesh_for_events
                                .send_sync_pull_response(
                                    &from_node_id,
                                    &tentaflow_protocol::cbor::encode(&response)
                                        .expect("encode pull response"),
                                )
                                .await
                        }
                        MeshSyncPullResult::Snapshot(response) => {
                            mesh_for_events
                                .send_sync_snapshot_response(
                                    &from_node_id,
                                    &tentaflow_protocol::cbor::encode(&response)
                                        .expect("encode snapshot response"),
                                )
                                .await
                        }
                    }
                    .expect("send pull result");
                }
                Ok(IrohMeshEvent::SyncPullResponseReceived { from_node_id, data }) => {
                    let payload = tentaflow_protocol::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncPullResponsePayload,
                    >(&data)
                    .expect("decode sync pull response");
                    let Some(ack) = tentaflow_core::sync::runtime::handle_pull_response_payload(
                        &from_node_id,
                        payload,
                    )
                    .expect("handle pull response") else {
                        continue;
                    };
                    let bytes = tentaflow_protocol::cbor::encode(&ack).expect("encode ack");
                    mesh_for_events
                        .send_sync_ack(&from_node_id, &bytes)
                        .await
                        .expect("send pull ack");
                }
                Ok(IrohMeshEvent::SyncSnapshotPullReceived { from_node_id, data }) => {
                    let payload = tentaflow_protocol::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncSnapshotPullPayload,
                    >(&data)
                    .expect("decode sync snapshot pull");
                    let Some(response) =
                        tentaflow_core::sync::runtime::handle_snapshot_pull_payload(
                            &from_node_id,
                            payload,
                        )
                        .expect("handle snapshot pull")
                    else {
                        continue;
                    };
                    let bytes = tentaflow_protocol::cbor::encode(&response)
                        .expect("encode snapshot response");
                    mesh_for_events
                        .send_sync_snapshot_response(&from_node_id, &bytes)
                        .await
                        .expect("send snapshot response");
                }
                Ok(IrohMeshEvent::SyncSnapshotResponseReceived { from_node_id, data }) => {
                    let payload = tentaflow_protocol::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncSnapshotResponsePayload,
                    >(&data)
                    .expect("decode sync snapshot response");
                    let Some(ack) =
                        tentaflow_core::sync::runtime::handle_snapshot_response_payload(
                            &from_node_id,
                            payload,
                        )
                        .expect("handle snapshot response")
                    else {
                        continue;
                    };
                    let bytes =
                        tentaflow_protocol::cbor::encode(&ack).expect("encode snapshot ack");
                    mesh_for_events
                        .send_sync_ack(&from_node_id, &bytes)
                        .await
                        .expect("send snapshot ack");
                }
                Ok(IrohMeshEvent::StorageProxyRequestReceived {
                    from_node_id,
                    payload,
                }) => {
                    tentaflow_core::services::storage_proxy::handle_request(
                        mesh_for_events.as_ref().clone(),
                        db_for_events.clone(),
                        local_node_id_for_events.clone(),
                        from_node_id,
                        payload,
                    )
                    .await;
                }
                Ok(IrohMeshEvent::StorageProxyResponseReceived { payload, .. }) => {
                    tentaflow_core::services::storage_proxy::storage_proxy_client()
                        .handle_response(payload);
                }
                Ok(IrohMeshEvent::PeerConnected { .. }) => {}
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    let addr = mesh
        .endpoint()
        .bound_sockets()
        .into_iter()
        .find(|addr| addr.is_ipv4())
        .expect("ipv4 bound socket");
    println!(
        "TF4 READY {} {} {}",
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
            println!("TF4 OK STOP");
            break;
        }
        match handle_child_command(&line, &db, &local_node_id, &security, &mesh).await {
            Ok(response) => println!("TF4 OK {response}"),
            Err(error) => println!("TF4 ERR {error}"),
        }
        std::io::stdout().flush().expect("command flush");
    }
}

async fn handle_child_command(
    line: &str,
    db: &tentaflow_core::db::DbPool,
    local_node_id: &str,
    security: &MeshSecurity,
    mesh: &IrohMeshManager,
) -> anyhow::Result<String> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        ["TRUST", node_id, public_key] => {
            security.add_trusted_key(node_id, public_key, "process-e2e", None)?;
            Ok("TRUST".to_string())
        }
        ["CONNECT", node_id, addr] => {
            let addr = addr.parse::<SocketAddr>()?;
            mesh.connect_to_peer_direct(node_id, addr).await?;
            wait_connected(mesh, node_id).await?;
            Ok("CONNECT".to_string())
        }
        ["SEED_RECEIVER"] => {
            seed_receiver(db, local_node_id, &security.public_key_hex())?;
            Ok("SEED_RECEIVER".to_string())
        }
        ["SEED_RECEIVER_CORE_SUITE"] => {
            seed_receiver_core_suite(db, local_node_id, &security.public_key_hex())?;
            Ok("SEED_RECEIVER_CORE_SUITE".to_string())
        }
        ["SEED_SQL_RECEIVER"] => {
            seed_sql_receiver(db, local_node_id, &security.public_key_hex())?;
            Ok("SEED_SQL_RECEIVER".to_string())
        }
        ["SEED_SQL_CENTRAL_CLIENT", authority_node_id, authority_public_key] => {
            seed_sql_central_client(
                db,
                local_node_id,
                &security.public_key_hex(),
                authority_node_id,
                authority_public_key,
            )?;
            Ok("SEED_SQL_CENTRAL_CLIENT".to_string())
        }
        ["SEED_SOURCE", count, rest @ ..] => {
            let count = count.parse::<usize>()?;
            anyhow::ensure!(rest.len() == count * 2, "target tuple count mismatch");
            seed_source(db, rest)?;
            Ok("SEED_SOURCE".to_string())
        }
        ["SEED_SOURCE_CORE_SUITE", count, rest @ ..] => {
            let count = count.parse::<usize>()?;
            anyhow::ensure!(rest.len() == count * 2, "target tuple count mismatch");
            seed_source_core_suite(db, rest)?;
            Ok("SEED_SOURCE_CORE_SUITE".to_string())
        }
        ["SEED_SQL_SOURCE", count, allowed_count, rest @ ..] => {
            let count = count.parse::<usize>()?;
            let allowed_count = allowed_count.parse::<usize>()?;
            anyhow::ensure!(rest.len() == count * 2, "target tuple count mismatch");
            anyhow::ensure!(allowed_count <= count, "allowed target count mismatch");
            seed_sql_source(
                db,
                local_node_id,
                &security.public_key_hex(),
                rest,
                allowed_count,
            )?;
            Ok("SEED_SQL_SOURCE".to_string())
        }
        ["SEED_SOURCE_ALLOWED", count, allowed_count, rest @ ..] => {
            let count = count.parse::<usize>()?;
            let allowed_count = allowed_count.parse::<usize>()?;
            anyhow::ensure!(rest.len() == count * 2, "target tuple count mismatch");
            anyhow::ensure!(allowed_count <= count, "allowed target count mismatch");
            seed_source_with_allowed_targets(db, rest, allowed_count, true)?;
            Ok("SEED_SOURCE_ALLOWED".to_string())
        }
        ["GRANT_SOURCE_TARGET", node_id] => {
            grant_source_target(db, node_id)?;
            Ok("GRANT_SOURCE_TARGET".to_string())
        }
        ["SEED_PEER", node_id, public_key] => {
            seed_peer(db, node_id, public_key)?;
            Ok("SEED_PEER".to_string())
        }
        ["READ_FLOW_NAME"] => {
            let name = repository::get_flow(db, FLOW_ID)?
                .map(|flow| flow.name)
                .unwrap_or_else(|| "<none>".to_string());
            Ok(format!("READ_FLOW_NAME {name}"))
        }
        ["RECORD_FLOW_NAMED", name] => {
            let capture = core_flow_capture_id(FLOW_ID, name);
            // Mirror production: the capture transaction that triggers a core sync
            // op ALSO writes the local row and stamps `core_resource_versions` with
            // the same HLC. The synthetic record path skips that, so do it here —
            // otherwise the authoring node never registers its own write in the
            // LWW gate and would later accept an OLDER inbound op.
            local_materialize_flow(db, &capture, name)?;
            let result = tentaflow_core::sync::runtime::record_core_capture(capture)?
                .expect("runtime initialized");
            Ok(format!("RECORD_FLOW_NAMED {}", result.op_id.to_hex()))
        }
        ["RECORD_FLOW"] => {
            let result = tentaflow_core::sync::runtime::record_core_capture(core_flow_capture())?
                .expect("runtime initialized");
            Ok(format!("RECORD_FLOW {}", result.op_id.to_hex()))
        }
        ["RECORD_FLOW_ID", resource_id, name] => {
            let result = tentaflow_core::sync::runtime::record_core_capture(core_flow_capture_id(
                resource_id,
                name,
            ))?
            .expect("runtime initialized");
            Ok(format!("RECORD_FLOW_ID {}", result.op_id.to_hex()))
        }
        ["RECORD_CORE_SUITE"] => {
            let op_ids = record_core_suite()?
                .into_iter()
                .map(|op_id| op_id.to_hex())
                .collect::<Vec<_>>()
                .join(" ");
            Ok(format!("RECORD_CORE_SUITE {op_ids}"))
        }
        ["RECORD_SQL_INSERT", name] => {
            let result = tentaflow_core::sync::runtime::record_sql_capture(sql_capture(
                SqlWriteAction::Insert,
                name,
            ))?
            .expect("runtime initialized");
            Ok(format!("RECORD_SQL_INSERT {}", result.op_id.to_hex()))
        }
        ["RECORD_SQL_UPDATE", name] => {
            let result = tentaflow_core::sync::runtime::record_sql_capture(sql_capture(
                SqlWriteAction::Update,
                name,
            ))?
            .expect("runtime initialized");
            Ok(format!("RECORD_SQL_UPDATE {}", result.op_id.to_hex()))
        }
        ["LOCAL_SQL_INSERT", name] => {
            local_sql_insert(name)?;
            Ok("LOCAL_SQL_INSERT".to_string())
        }
        ["REMOTE_SQL_QUERY", authority_node_id, expected_name] => {
            remote_sql_query(mesh, local_node_id, authority_node_id, expected_name).await?;
            Ok("REMOTE_SQL_QUERY".to_string())
        }
        ["REMOTE_SQL_EXEC", authority_node_id, name] => {
            remote_sql_exec(mesh, local_node_id, authority_node_id, name).await?;
            Ok("REMOTE_SQL_EXEC".to_string())
        }
        ["BUILD_SQL_SNAPSHOT", sequence] => {
            let sequence = sequence.parse::<u64>()?;
            let snapshot = tentaflow_core::sync::runtime::build_sql_snapshot_package(
                SNAPSHOT_PARTITION,
                Some(sequence),
            )?
            .expect("snapshot built");
            Ok(format!(
                "BUILD_SQL_SNAPSHOT {} {}",
                snapshot.up_to_sequence,
                snapshot.snapshot_id.as_str()
            ))
        }
        ["ASSERT_NO_PAYLOAD", target] => {
            anyhow::ensure!(
                tentaflow_core::sync::runtime::build_push_payload_for_target(target, 32)?.is_none(),
                "unexpected push payload for {target}"
            );
            Ok("ASSERT_NO_PAYLOAD".to_string())
        }
        ["PUSH", target] => {
            let payload = tentaflow_core::sync::runtime::build_push_payload_for_target(target, 32)?
                .expect("push payload");
            let bytes = tentaflow_protocol::cbor::encode(&payload).expect("encode push");
            mesh.send_sync_push(target, &bytes).await?;
            Ok("PUSH".to_string())
        }
        ["PUSH_IF_PENDING", target] => {
            if let Some(payload) =
                tentaflow_core::sync::runtime::build_push_payload_for_target(target, 32)?
            {
                let bytes = tentaflow_protocol::cbor::encode(&payload).expect("encode push");
                mesh.send_sync_push(target, &bytes).await?;
            }
            Ok("PUSH_IF_PENDING".to_string())
        }
        ["BACKFILL_GRANTS"] => {
            let count = tentaflow_core::sync::runtime::backfill_outbox_for_permission_grants()?
                .unwrap_or(0);
            Ok(format!("BACKFILL_GRANTS {count}"))
        }
        ["SEND_REPAIR", peer] => {
            send_repair_pull(mesh, peer).await?;
            Ok("SEND_REPAIR".to_string())
        }
        ["APPLY_INBOX"] => {
            let applied = tentaflow_core::sync::runtime::apply_unapplied_inbox(256)?.unwrap_or(0);
            Ok(format!("APPLY_INBOX {applied}"))
        }
        ["READ_FLOW_HLC"] => {
            let conn = db
                .read()
                .map_err(|error| anyhow::anyhow!("db lock failed: {error}"))?;
            let hlc = conn
                .query_row(
                    "SELECT hlc_wall, hlc_logical, hlc_node FROM core_resource_versions \
                     WHERE resource_type = 'core.flow' AND resource_id = ?1",
                    rusqlite::params![FLOW_ID],
                    |row| {
                        Ok(format!(
                            "{}:{}:{}",
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?
                        ))
                    },
                )
                .optional()?
                .unwrap_or_else(|| "<none>".to_string());
            Ok(format!("READ_FLOW_HLC {hlc}"))
        }
        ["SEND_SNAPSHOT", peer, sequence, snapshot_id] => {
            send_snapshot_pull(mesh, peer, sequence.parse()?, snapshot_id).await?;
            Ok("SEND_SNAPSHOT".to_string())
        }
        ["ASSERT_REPAIR_REDACTED", peer] => {
            assert_repair_redacted(peer)?;
            Ok("ASSERT_REPAIR_REDACTED".to_string())
        }
        ["ASSERT_SNAPSHOT_DENIED", peer, sequence, snapshot_id] => {
            assert_snapshot_denied(peer, sequence.parse()?, snapshot_id)?;
            Ok("ASSERT_SNAPSHOT_DENIED".to_string())
        }
        ["WAIT_FLOW"] => {
            wait_for_flow(db).await?;
            Ok("WAIT_FLOW".to_string())
        }
        ["WAIT_FLOW_NAME", name] => {
            wait_for_flow_name(db, name).await?;
            Ok("WAIT_FLOW_NAME".to_string())
        }
        ["WAIT_CORE_SUITE"] => {
            wait_for_core_suite(db).await?;
            Ok("WAIT_CORE_SUITE".to_string())
        }
        ["WAIT_SQL_NAME", name] => {
            wait_for_sql_name(name).await?;
            Ok("WAIT_SQL_NAME".to_string())
        }
        ["ASSERT_NO_SQL"] => {
            assert_no_sql().await?;
            Ok("ASSERT_NO_SQL".to_string())
        }
        ["WAIT_SQL_CONFLICT"] => {
            wait_for_sql_conflict().await?;
            Ok("WAIT_SQL_CONFLICT".to_string())
        }
        ["ASSERT_NO_FLOW"] => {
            assert_no_flow(db).await?;
            Ok("ASSERT_NO_FLOW".to_string())
        }
        ["WAIT_ACKS", op_id, count] => {
            wait_for_acks(op_id, count.parse()?).await?;
            Ok("WAIT_ACKS".to_string())
        }
        ["ASSERT_NO_PENDING", op_id] => {
            wait_for_acks(op_id, 3).await?;
            Ok("ASSERT_NO_PENDING".to_string())
        }
        _ => anyhow::bail!("unknown command: {line}"),
    }
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

async fn send_repair_pull(mesh: &IrohMeshManager, peer: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let payloads =
            tentaflow_core::sync::runtime::build_repair_pull_payloads_for_peer(peer, 16, 256)?;
        if payloads.is_empty() {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }
        for payload in payloads {
            let bytes = tentaflow_protocol::cbor::encode(&payload).expect("encode repair pull");
            mesh.send_sync_pull(peer, &bytes).await?;
        }
        return Ok(());
    }
    anyhow::bail!("repair pull not queued for {peer}")
}

async fn send_snapshot_pull(
    mesh: &IrohMeshManager,
    peer: &str,
    up_to_sequence: u64,
    snapshot_id: &str,
) -> anyhow::Result<()> {
    let payload = tentaflow_core::sync::runtime::build_snapshot_pull_payload(
        SNAPSHOT_PARTITION,
        up_to_sequence,
        snapshot_id,
        true,
        64,
    )?
    .expect("runtime initialized");
    let bytes = tentaflow_protocol::cbor::encode(&payload).expect("encode snapshot pull");
    mesh.send_sync_snapshot_pull(peer, &bytes).await?;
    Ok(())
}

fn assert_repair_redacted(peer: &str) -> anyhow::Result<()> {
    // Pull the local node's own chain (it authored the snapshot partition) as an
    // unauthorized requester. The pull is no longer hard-denied: the serving side
    // REDACTS every op the requester is not a sync target for, so the chain stays
    // dense (the requester can advance its frontier) but leaks no resource body.
    // Assert the response carries operations and every one is redacted.
    let target_node_id = tentaflow_core::sync::runtime::local_node_id()
        .ok_or_else(|| anyhow::anyhow!("sync runtime not started"))?;
    let payload = tentaflow_protocol::mesh::MeshSyncPullPayload {
        from_node_id: peer.to_string(),
        target_node_id,
        from_node_seq: 1,
        limit: 64,
    };
    let result = tentaflow_core::sync::runtime::handle_pull_payload(peer, payload)?
        .ok_or_else(|| anyhow::anyhow!("repair pull returned no result"))?;
    let MeshSyncPullResult::Operations(response) = result else {
        anyhow::bail!("repair pull must serve operations, not a snapshot");
    };
    anyhow::ensure!(
        !response.operations.is_empty(),
        "repair pull served no operations to redact"
    );
    for op in &response.operations {
        anyhow::ensure!(
            op.redacted.is_some(),
            "unauthorized requester must receive only redacted ops"
        );
        anyhow::ensure!(
            op.operation.is_empty() && op.partition_id.is_empty(),
            "redacted op must withhold body and partition"
        );
    }
    Ok(())
}

fn assert_snapshot_denied(
    peer: &str,
    up_to_sequence: u64,
    snapshot_id: &str,
) -> anyhow::Result<()> {
    let payload = tentaflow_protocol::mesh::MeshSyncSnapshotPullPayload {
        from_node_id: peer.to_string(),
        partition_id: SNAPSHOT_PARTITION.to_string(),
        up_to_sequence,
        snapshot_id: snapshot_id.to_string(),
        include_tail: true,
        tail_limit: 64,
    };
    let error = match tentaflow_core::sync::runtime::handle_snapshot_pull_payload(peer, payload) {
        Ok(_) => anyhow::bail!("snapshot pull must be denied"),
        Err(error) => error,
    };
    anyhow::ensure!(
        error.to_string().contains("is not a sync target"),
        "unexpected snapshot denial: {error}"
    );
    Ok(())
}

fn seed_receiver(
    db: &tentaflow_core::db::DbPool,
    local_node_id: &str,
    public_key: &str,
) -> anyhow::Result<()> {
    repository::upsert_sync_node_identity(
        db,
        local_node_id,
        public_key,
        "ed25519",
        "Process E2E Receiver",
        "authority",
        "trusted",
        None,
        "authority",
    )?;
    repository::upsert_sync_policy(
        db,
        "process-e2e-receiver-core-flow",
        "org-default",
        CORE_SYNC_ADDON_ID,
        Some("core.flow"),
        None,
        "authority_write",
        Some(local_node_id),
        None,
        true,
    )?;
    Ok(())
}

fn seed_receiver_core_suite(
    db: &tentaflow_core::db::DbPool,
    local_node_id: &str,
    public_key: &str,
) -> anyhow::Result<()> {
    repository::upsert_sync_node_identity(
        db,
        local_node_id,
        public_key,
        "ed25519",
        "Process E2E Receiver",
        "authority",
        "trusted",
        None,
        "authority",
    )?;
    for kind in core_suite_kinds() {
        let descriptor = descriptor_for_kind(kind);
        repository::upsert_sync_policy(
            db,
            &format!("process-e2e-receiver-{}", descriptor.resource_type),
            "org-default",
            CORE_SYNC_ADDON_ID,
            Some(descriptor.resource_type),
            None,
            "authority_write",
            Some(local_node_id),
            None,
            true,
        )?;
    }
    Ok(())
}

fn seed_sql_receiver(
    db: &tentaflow_core::db::DbPool,
    local_node_id: &str,
    public_key: &str,
) -> anyhow::Result<()> {
    repository::upsert_sync_node_identity(
        db,
        local_node_id,
        public_key,
        "ed25519",
        "Process SQL Receiver",
        "authority",
        "trusted",
        None,
        "authority",
    )?;
    repository::upsert_sync_policy(
        db,
        "process-sql-receiver",
        "org-default",
        SNAPSHOT_ADDON_ID,
        Some("person"),
        None,
        "authority_write",
        Some(local_node_id),
        None,
        true,
    )?;
    open_sql_table()?;
    reset_sql_table()?;
    Ok(())
}

fn seed_sql_central_client(
    db: &tentaflow_core::db::DbPool,
    local_node_id: &str,
    local_public_key: &str,
    authority_node_id: &str,
    authority_public_key: &str,
) -> anyhow::Result<()> {
    repository::upsert_sync_node_identity(
        db,
        local_node_id,
        local_public_key,
        "ed25519",
        "Process SQL Central Client",
        "desktop",
        "trusted",
        None,
        "standard",
    )?;
    repository::upsert_sync_node_identity(
        db,
        authority_node_id,
        authority_public_key,
        "ed25519",
        "Process SQL Authority",
        "server",
        "trusted",
        None,
        "authority",
    )?;
    repository::upsert_sync_policy(
        db,
        "process-sql-central-client",
        "org-default",
        SNAPSHOT_ADDON_ID,
        Some("person"),
        None,
        "replicated_by_permission",
        Some(authority_node_id),
        None,
        true,
    )?;
    open_sql_table()?;
    reset_sql_table()?;
    Ok(())
}

fn seed_source(db: &tentaflow_core::db::DbPool, targets: &[&str]) -> anyhow::Result<()> {
    seed_source_with_allowed_targets(db, targets, targets.len() / 2, false)
}

fn seed_source_core_suite(db: &tentaflow_core::db::DbPool, targets: &[&str]) -> anyhow::Result<()> {
    for kind in core_suite_kinds() {
        let descriptor = descriptor_for_kind(kind);
        repository::upsert_sync_policy(
            db,
            &format!("process-e2e-source-{}", descriptor.resource_type),
            "org-default",
            CORE_SYNC_ADDON_ID,
            Some(descriptor.resource_type),
            None,
            "replicated_by_permission",
            None,
            None,
            true,
        )?;
    }
    for (idx, pair) in targets.chunks_exact(2).enumerate() {
        let node_id = pair[0];
        let public_key = pair[1];
        repository::upsert_sync_node_identity(
            db,
            node_id,
            public_key,
            "ed25519",
            &format!("Process E2E Receiver {idx}"),
            "server",
            "trusted",
            None,
            "standard",
        )?;
        grant_core_suite_target(db, node_id)?;
    }
    Ok(())
}

fn seed_sql_source(
    db: &tentaflow_core::db::DbPool,
    local_node_id: &str,
    local_public_key: &str,
    targets: &[&str],
    allowed_count: usize,
) -> anyhow::Result<()> {
    repository::upsert_sync_node_identity(
        db,
        local_node_id,
        local_public_key,
        "ed25519",
        "Process SQL Authority",
        "server",
        "trusted",
        None,
        "authority",
    )?;
    repository::upsert_sync_policy(
        db,
        "process-sql-source",
        "org-default",
        SNAPSHOT_ADDON_ID,
        Some("person"),
        None,
        "replicated_by_permission",
        Some(local_node_id),
        None,
        true,
    )?;
    for (idx, pair) in targets.chunks_exact(2).enumerate() {
        let node_id = pair[0];
        let public_key = pair[1];
        repository::upsert_sync_node_identity(
            db,
            node_id,
            public_key,
            "ed25519",
            &format!("Process SQL Receiver {idx}"),
            "server",
            "trusted",
            None,
            "standard",
        )?;
        if idx < allowed_count {
            repository::grant_sync_explicit_share(
                db,
                "org-default",
                SNAPSHOT_ADDON_ID,
                "person",
                "person-1",
                "node",
                node_id,
                "sync_receive",
                None,
            )?;
        } else {
            grant_storage_proxy_target(db, node_id)?;
        }
    }
    open_sql_table()?;
    reset_sql_table()?;
    Ok(())
}

fn grant_storage_proxy_target(
    db: &tentaflow_core::db::DbPool,
    node_id: &str,
) -> anyhow::Result<()> {
    for action in ["read", "write"] {
        repository::grant_sync_explicit_share(
            db,
            "org-default",
            SNAPSHOT_ADDON_ID,
            "person",
            "person-1",
            "node",
            node_id,
            action,
            None,
        )?;
    }
    Ok(())
}

fn seed_source_with_allowed_targets(
    db: &tentaflow_core::db::DbPool,
    targets: &[&str],
    allowed_count: usize,
    scope_with_acl: bool,
) -> anyhow::Result<()> {
    repository::upsert_sync_policy(
        db,
        "process-e2e-source-core-flow",
        "org-default",
        CORE_SYNC_ADDON_ID,
        Some("core.flow"),
        None,
        "replicated_by_permission",
        None,
        None,
        true,
    )?;
    // `core.flow` is a durable org-scoped core resource, so by default it
    // blanket-replicates to every trusted node. To actually gate it per-node
    // (only explicitly shared receivers), a resource-level ACL must scope it to
    // `explicit_share` — once an ACL row exists the blanket allow yields to the
    // per-share checks.
    if scope_with_acl {
        repository::upsert_sync_resource_acl(
            db,
            "org-default",
            CORE_SYNC_ADDON_ID,
            "core.flow",
            FLOW_ID,
            None,
            None,
            None,
            None,
            "explicit_share",
        )?;
    }
    for (idx, pair) in targets.chunks_exact(2).enumerate() {
        let node_id = pair[0];
        let public_key = pair[1];
        repository::upsert_sync_node_identity(
            db,
            node_id,
            public_key,
            "ed25519",
            &format!("Process E2E Receiver {idx}"),
            "server",
            "trusted",
            None,
            "standard",
        )?;
        if idx < allowed_count {
            grant_source_target(db, node_id)?;
        }
    }
    Ok(())
}

// Registers a remote node as a trusted standard sync peer and ensures the
// `core.flow` replication policy exists, so the local node fans out its own
// `core.flow` writes to that peer (blanket core replication to trusted nodes).
fn seed_peer(
    db: &tentaflow_core::db::DbPool,
    node_id: &str,
    public_key: &str,
) -> anyhow::Result<()> {
    repository::upsert_sync_policy(
        db,
        "process-e2e-converge-core-flow",
        "org-default",
        CORE_SYNC_ADDON_ID,
        Some("core.flow"),
        None,
        "replicated_by_permission",
        None,
        None,
        true,
    )?;
    repository::upsert_sync_node_identity(
        db,
        node_id,
        public_key,
        "ed25519",
        "Process E2E Peer",
        "server",
        "trusted",
        None,
        "standard",
    )?;
    Ok(())
}

fn grant_source_target(db: &tentaflow_core::db::DbPool, node_id: &str) -> anyhow::Result<()> {
    repository::grant_sync_explicit_share(
        db,
        "org-default",
        CORE_SYNC_ADDON_ID,
        "core.flow",
        FLOW_ID,
        "node",
        node_id,
        "sync_receive",
        None,
    )?;
    Ok(())
}

fn grant_core_suite_target(db: &tentaflow_core::db::DbPool, node_id: &str) -> anyhow::Result<()> {
    for (kind, resource_id) in core_suite_resources() {
        let descriptor = descriptor_for_kind(kind);
        repository::grant_sync_explicit_share(
            db,
            "org-default",
            CORE_SYNC_ADDON_ID,
            descriptor.resource_type,
            resource_id,
            "node",
            node_id,
            "sync_receive",
            None,
        )?;
    }
    Ok(())
}

fn core_suite_kinds() -> [CoreSyncResourceKind; 8] {
    [
        CoreSyncResourceKind::Organization,
        CoreSyncResourceKind::UserAccount,
        CoreSyncResourceKind::UserGroup,
        CoreSyncResourceKind::GroupMember,
        CoreSyncResourceKind::Role,
        CoreSyncResourceKind::OrgMembership,
        CoreSyncResourceKind::Flow,
        CoreSyncResourceKind::FlowModelBinding,
    ]
}

fn core_suite_resources() -> [(CoreSyncResourceKind, &'static str); 8] {
    [
        (CoreSyncResourceKind::Organization, SUITE_ORG_ID),
        (CoreSyncResourceKind::UserAccount, SUITE_USER_ID),
        (CoreSyncResourceKind::UserGroup, SUITE_GROUP_ID),
        (CoreSyncResourceKind::GroupMember, "20102:20101"),
        (CoreSyncResourceKind::Role, SUITE_ROLE_ID),
        (CoreSyncResourceKind::OrgMembership, "org-default:20101"),
        (CoreSyncResourceKind::Flow, SUITE_FLOW_ID),
        (CoreSyncResourceKind::FlowModelBinding, SUITE_BINDING_ID),
    ]
}

fn core_flow_capture() -> CoreWriteCapture {
    core_flow_capture_id(FLOW_ID, "Process Four Node Flow")
}

// Writes the local flow row and stamps `core_resource_versions` with the
// capture HLC, mirroring the atomic capture+row+version write the production
// capture transaction performs for a core sync write.
fn local_materialize_flow(
    db: &tentaflow_core::db::DbPool,
    capture: &CoreWriteCapture,
    name: &str,
) -> anyhow::Result<()> {
    let conn = db
        .write()
        .map_err(|error| anyhow::anyhow!("db lock failed: {error}"))?;
    conn.execute(
        "INSERT INTO flows (id, name, description, is_default, service_type, flow_json, status, published_model_name) \
         VALUES (?1, ?2, NULL, 0, NULL, ?3, 'active', NULL) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, flow_json = excluded.flow_json, \
             status = excluded.status, updated_at = datetime('now')",
        rusqlite::params![FLOW_ID, name, r#"{"nodes":[]}"#],
    )?;
    conn.execute(
        "INSERT INTO core_resource_versions (resource_type, resource_id, hlc_wall, hlc_logical, hlc_node) \
         VALUES ('core.flow', ?1, ?2, ?3, ?4) \
         ON CONFLICT(resource_type, resource_id) DO UPDATE SET \
             hlc_wall = excluded.hlc_wall, hlc_logical = excluded.hlc_logical, hlc_node = excluded.hlc_node",
        rusqlite::params![
            FLOW_ID,
            capture.hlc.wall_time_ms,
            capture.hlc.logical as i64,
            capture.hlc.node_id,
        ],
    )?;
    Ok(())
}

fn core_flow_capture_id(resource_id: &str, name: &str) -> CoreWriteCapture {
    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), FieldValue::String(name.to_string()));
    fields.insert("is_default".to_string(), FieldValue::Bool(false));
    fields.insert(
        "flow_json".to_string(),
        FieldValue::String(r#"{"nodes":[]}"#.to_string()),
    );
    fields.insert(
        "status".to_string(),
        FieldValue::String("active".to_string()),
    );
    CoreWriteCapture::new(
        CoreSyncResourceKind::Flow,
        "org-default",
        resource_id,
        SqlWriteAction::Insert,
        fields,
        Some("1".to_string()),
        tentaflow_core::sync::runtime::core_hlc_now(),
        tentaflow_core::sync::runtime::core_epoch(),
    )
}

fn record_core_suite() -> anyhow::Result<Vec<OperationId>> {
    let captures = core_suite_captures();
    let mut op_ids = Vec::with_capacity(captures.len());
    for capture in captures {
        let result = tentaflow_core::sync::runtime::record_core_capture(capture)?
            .expect("runtime initialized");
        op_ids.push(result.op_id);
    }
    Ok(op_ids)
}

fn core_suite_captures() -> Vec<CoreWriteCapture> {
    let mut captures = Vec::new();

    let mut organization = BTreeMap::new();
    organization.insert(
        "name".to_string(),
        FieldValue::String("Process Sync Org".to_string()),
    );
    organization.insert(
        "slug".to_string(),
        FieldValue::String("process-sync-org".to_string()),
    );
    organization.insert(
        "status".to_string(),
        FieldValue::String("active".to_string()),
    );
    captures.push(core_capture_for(
        CoreSyncResourceKind::Organization,
        SUITE_ORG_ID,
        organization,
    ));

    let mut role = BTreeMap::new();
    role.insert(
        "name".to_string(),
        FieldValue::String("Process Sync Role".to_string()),
    );
    role.insert(
        "permissions_json".to_string(),
        FieldValue::String(r#"["contacts.read","flows.write"]"#.to_string()),
    );
    captures.push(core_capture_for(
        CoreSyncResourceKind::Role,
        SUITE_ROLE_ID,
        role,
    ));

    let mut user = BTreeMap::new();
    user.insert(
        "username".to_string(),
        FieldValue::String("process-sync-user".to_string()),
    );
    user.insert(
        "display_name".to_string(),
        FieldValue::String("Process Sync User".to_string()),
    );
    user.insert(
        "email".to_string(),
        FieldValue::String("process-sync@example.test".to_string()),
    );
    user.insert("is_active".to_string(), FieldValue::Bool(true));
    user.insert("is_admin".to_string(), FieldValue::Bool(false));
    user.insert("role".to_string(), FieldValue::String("user".to_string()));
    captures.push(core_capture_for(
        CoreSyncResourceKind::UserAccount,
        SUITE_USER_ID,
        user,
    ));

    let mut group = BTreeMap::new();
    group.insert(
        "name".to_string(),
        FieldValue::String("Process Sync Group".to_string()),
    );
    group.insert(
        "description".to_string(),
        FieldValue::String("Four-node synchronized group".to_string()),
    );
    captures.push(core_capture_for(
        CoreSyncResourceKind::UserGroup,
        SUITE_GROUP_ID,
        group,
    ));

    let mut group_member = BTreeMap::new();
    group_member.insert(
        "group_id".to_string(),
        FieldValue::String(SUITE_GROUP_ID.to_string()),
    );
    group_member.insert(
        "user_id".to_string(),
        FieldValue::String(SUITE_USER_ID.to_string()),
    );
    captures.push(core_capture_for(
        CoreSyncResourceKind::GroupMember,
        "20102:20101",
        group_member,
    ));

    let mut membership = BTreeMap::new();
    membership.insert(
        "org_id".to_string(),
        FieldValue::String("org-default".to_string()),
    );
    membership.insert(
        "user_id".to_string(),
        FieldValue::String(SUITE_USER_ID.to_string()),
    );
    membership.insert(
        "role_id".to_string(),
        FieldValue::String(SUITE_ROLE_ID.to_string()),
    );
    membership.insert(
        "granted_by".to_string(),
        FieldValue::String("process-e2e".to_string()),
    );
    captures.push(core_capture_for(
        CoreSyncResourceKind::OrgMembership,
        "org-default:20101",
        membership,
    ));

    captures.push(core_suite_flow_capture());

    let mut binding = BTreeMap::new();
    binding.insert(
        "flow_id".to_string(),
        FieldValue::String(SUITE_FLOW_ID.to_string()),
    );
    binding.insert(
        "model_pattern".to_string(),
        FieldValue::String(SUITE_MODEL_PATTERN.to_string()),
    );
    binding.insert("priority".to_string(), FieldValue::I64(10));
    captures.push(core_capture_for(
        CoreSyncResourceKind::FlowModelBinding,
        SUITE_BINDING_ID,
        binding,
    ));

    captures
}

fn core_capture_for(
    kind: CoreSyncResourceKind,
    resource_id: &str,
    fields: BTreeMap<String, FieldValue>,
) -> CoreWriteCapture {
    CoreWriteCapture::new(
        kind,
        "org-default",
        resource_id,
        SqlWriteAction::Insert,
        fields,
        Some("1".to_string()),
        tentaflow_core::sync::runtime::core_hlc_now(),
        tentaflow_core::sync::runtime::core_epoch(),
    )
}

fn core_suite_flow_capture() -> CoreWriteCapture {
    let mut fields = BTreeMap::new();
    fields.insert(
        "name".to_string(),
        FieldValue::String("Process Suite Flow".to_string()),
    );
    fields.insert(
        "description".to_string(),
        FieldValue::String("Flow synchronized in the four-process suite".to_string()),
    );
    fields.insert("is_default".to_string(), FieldValue::Bool(false));
    fields.insert(
        "service_type".to_string(),
        FieldValue::String("chat".to_string()),
    );
    fields.insert(
        "flow_json".to_string(),
        FieldValue::String(r#"{"nodes":[{"id":"trigger"}]}"#.to_string()),
    );
    fields.insert(
        "status".to_string(),
        FieldValue::String("active".to_string()),
    );
    fields.insert(
        "published_model_name".to_string(),
        FieldValue::String("process-suite-model".to_string()),
    );
    CoreWriteCapture::new(
        CoreSyncResourceKind::Flow,
        "org-default",
        SUITE_FLOW_ID,
        SqlWriteAction::Insert,
        fields,
        Some("1".to_string()),
        tentaflow_core::sync::runtime::core_hlc_now(),
        tentaflow_core::sync::runtime::core_epoch(),
    )
}

fn open_sql_table() -> anyhow::Result<()> {
    let pool = tentaflow_core::addon::storage_sql::open_addon_db("org-default", SNAPSHOT_ADDON_ID)
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    let conn = pool.get().map_err(|error| anyhow::anyhow!("{error:?}"))?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS contacts (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        [],
    )?;
    Ok(())
}

fn reset_sql_table() -> anyhow::Result<()> {
    let pool = tentaflow_core::addon::storage_sql::open_addon_db("org-default", SNAPSHOT_ADDON_ID)
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    let conn = pool.get().map_err(|error| anyhow::anyhow!("{error:?}"))?;
    conn.execute("DELETE FROM contacts", [])?;
    let _ = conn.execute("DELETE FROM __tentaflow_sync_conflicts", []);
    Ok(())
}

fn sql_capture(action: SqlWriteAction, name: &str) -> SqlWriteCapture {
    let (query, params) = match action {
        SqlWriteAction::Insert => (
            "INSERT INTO contacts (id, name) VALUES (?1, ?2)".to_string(),
            vec![JsonValue::from(1), JsonValue::String(name.to_string())],
        ),
        SqlWriteAction::Update => (
            "UPDATE contacts SET name = ?1 WHERE id = ?2".to_string(),
            vec![JsonValue::String(name.to_string()), JsonValue::from(1)],
        ),
        SqlWriteAction::Delete => (
            "DELETE FROM contacts WHERE id = ?1".to_string(),
            vec![JsonValue::from(1)],
        ),
    };
    SqlWriteCapture {
        capture_id: format!(
            "{}-{}-{}",
            SNAPSHOT_ADDON_ID,
            action.as_str(),
            name.replace(' ', "-")
        ),
        org_id: "org-default".to_string(),
        addon_id: SNAPSHOT_ADDON_ID.to_string(),
        table_name: "contacts".to_string(),
        action,
        resource_type: "person".to_string(),
        resource_id: "person-1".to_string(),
        query,
        params,
        rows_affected: 1,
        last_insert_id: 1,
        actor_user_id: Some("7".to_string()),
        created_at_ms: tentaflow_core::sync::runtime::now_ms(),
    }
}

fn local_sql_insert(name: &str) -> anyhow::Result<()> {
    open_sql_table()?;
    let pool = tentaflow_core::addon::storage_sql::open_addon_db("org-default", SNAPSHOT_ADDON_ID)
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    let conn = pool.get().map_err(|error| anyhow::anyhow!("{error:?}"))?;
    conn.execute(
        "INSERT INTO contacts (id, name) VALUES (?1, ?2)",
        rusqlite::params![1_i64, name],
    )?;
    Ok(())
}

async fn remote_sql_query(
    mesh: &IrohMeshManager,
    local_node_id: &str,
    authority_node_id: &str,
    expected_name: &str,
) -> anyhow::Result<()> {
    let request = StorageProxyRequestPayload {
        request_id: String::new(),
        from_node_id: local_node_id.to_string(),
        org_id: "org-default".to_string(),
        addon_id: SNAPSHOT_ADDON_ID.to_string(),
        resource_type: "person".to_string(),
        resource_id: "person-1".to_string(),
        actor_user_id: Some("7".to_string()),
        kind: StorageProxyRequestKind::SqlQuery {
            query: "SELECT name FROM contacts WHERE id = ?1".to_string(),
            params: vec![StorageValueWire::I64(1)],
            one: true,
            limit: Some(1),
        },
    };
    let response = tentaflow_core::services::storage_proxy::remote_sql_query(
        mesh,
        authority_node_id,
        request,
        Duration::from_secs(10),
    )
    .await?;
    let actual = response
        .get("row")
        .and_then(|row| row.as_array())
        .and_then(|row| row.first())
        .and_then(|value| value.as_str());
    anyhow::ensure!(
        actual == Some(expected_name),
        "remote sql query returned {actual:?}, expected {expected_name}"
    );
    Ok(())
}

async fn remote_sql_exec(
    mesh: &IrohMeshManager,
    local_node_id: &str,
    authority_node_id: &str,
    name: &str,
) -> anyhow::Result<()> {
    let request = StorageProxyRequestPayload {
        request_id: String::new(),
        from_node_id: local_node_id.to_string(),
        org_id: "org-default".to_string(),
        addon_id: SNAPSHOT_ADDON_ID.to_string(),
        resource_type: "person".to_string(),
        resource_id: "person-1".to_string(),
        actor_user_id: Some("7".to_string()),
        kind: StorageProxyRequestKind::SqlExec {
            query: "UPDATE contacts SET name = ?1 WHERE id = ?2".to_string(),
            params: vec![
                StorageValueWire::Text(name.to_string()),
                StorageValueWire::I64(1),
            ],
        },
    };
    let (rows_affected, _) = tentaflow_core::services::storage_proxy::remote_sql_exec(
        mesh,
        authority_node_id,
        request,
        Duration::from_secs(10),
    )
    .await?;
    anyhow::ensure!(
        rows_affected == 1,
        "remote sql exec affected {rows_affected} rows"
    );
    Ok(())
}

async fn wait_for_flow(db: &tentaflow_core::db::DbPool) -> anyhow::Result<()> {
    wait_for_flow_name(db, "Process Four Node Flow").await
}

async fn wait_for_flow_name(
    db: &tentaflow_core::db::DbPool,
    expected_name: &str,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(flow) = repository::get_flow(db, FLOW_ID)? {
            if flow.name == expected_name {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("flow not materialized as {expected_name}")
}

async fn wait_for_core_suite(db: &tentaflow_core::db::DbPool) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut status = String::new();
    while Instant::now() < deadline {
        status = core_suite_status(db)?;
        if status == "complete" {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("core suite not materialized: {status}")
}

async fn wait_for_sql_name(expected: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        open_sql_table()?;
        let pool =
            tentaflow_core::addon::storage_sql::open_addon_db("org-default", SNAPSHOT_ADDON_ID)
                .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let value = {
            let conn = pool.get().map_err(|error| anyhow::anyhow!("{error:?}"))?;
            conn.query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
        };
        if value.as_deref() == Some(expected) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("sql name not materialized: expected {expected}")
}

async fn assert_no_sql() -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        open_sql_table()?;
        let pool =
            tentaflow_core::addon::storage_sql::open_addon_db("org-default", SNAPSHOT_ADDON_ID)
                .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let count = {
            let conn = pool.get().map_err(|error| anyhow::anyhow!("{error:?}"))?;
            conn.query_row("SELECT COUNT(*) FROM contacts", [], |row| {
                row.get::<_, i64>(0)
            })?
        };
        if count > 0 {
            anyhow::bail!("sql unexpectedly materialized: {count} row(s)");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

async fn wait_for_sql_conflict() -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        open_sql_table()?;
        let conflicts = tentaflow_core::addon::storage_sql_exec::list_sync_conflicts(
            "org-default",
            SNAPSHOT_ADDON_ID,
            Some("open"),
            10,
        )?;
        if !conflicts.is_empty() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("sql conflict not recorded")
}

fn core_suite_status(db: &tentaflow_core::db::DbPool) -> anyhow::Result<String> {
    let conn = db
        .read()
        .map_err(|error| anyhow::anyhow!("db lock failed: {error}"))?;
    let org_name = conn
        .query_row(
            "SELECT name FROM organizations WHERE org_id = ?1",
            rusqlite::params![SUITE_ORG_ID],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let username = conn
        .query_row(
            "SELECT username FROM user_accounts WHERE id = ?1",
            rusqlite::params![SUITE_USER_ID],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let group_name = conn
        .query_row(
            "SELECT name FROM user_groups WHERE id = ?1",
            rusqlite::params![SUITE_GROUP_ID],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let group_count = conn.query_row(
        "SELECT COUNT(*) FROM group_members WHERE group_id = ?1 AND user_id = ?2",
        rusqlite::params![SUITE_GROUP_ID, SUITE_USER_ID],
        |row| row.get::<_, i64>(0),
    )?;
    let role_name = conn
        .query_row(
            "SELECT name FROM roles WHERE role_id = ?1",
            rusqlite::params![SUITE_ROLE_ID],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let membership_count = conn.query_row(
        "SELECT COUNT(*) FROM org_memberships WHERE org_id = ?1 AND user_id = ?2 AND role_id = ?3",
        rusqlite::params!["org-default", SUITE_USER_ID, SUITE_ROLE_ID],
        |row| row.get::<_, i64>(0),
    )?;
    let flow_name = conn
        .query_row(
            "SELECT name FROM flows WHERE id = ?1",
            rusqlite::params![SUITE_FLOW_ID],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let binding_pattern = conn
        .query_row(
            "SELECT model_pattern FROM flow_model_bindings WHERE id = ?1",
            rusqlite::params![SUITE_BINDING_ID],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if org_name.as_deref() == Some("Process Sync Org")
        && username.as_deref() == Some("process-sync-user")
        && group_name.as_deref() == Some("Process Sync Group")
        && group_count == 1
        && role_name.as_deref() == Some("Process Sync Role")
        && membership_count == 1
        && flow_name.as_deref() == Some("Process Suite Flow")
        && binding_pattern.as_deref() == Some(SUITE_MODEL_PATTERN)
    {
        return Ok("complete".to_string());
    }
    Ok(format!(
        "org={org_name:?} user={username:?} group={group_name:?} group_count={group_count} \
         role={role_name:?} membership_count={membership_count} flow={flow_name:?} \
         binding={binding_pattern:?}"
    ))
}

async fn assert_no_flow(db: &tentaflow_core::db::DbPool) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if let Some(flow) = repository::get_flow(db, FLOW_ID)? {
            anyhow::bail!("flow unexpectedly materialized: {}", flow.name);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

async fn wait_for_acks(op_id: &str, expected: usize) -> anyhow::Result<()> {
    let operation_id = OperationId::from_hex(op_id)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let count = tentaflow_core::sync::runtime::acknowledged_outbox_count(operation_id)?
            .expect("runtime initialized");
        if count >= expected {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("acks not observed for {op_id}")
}
