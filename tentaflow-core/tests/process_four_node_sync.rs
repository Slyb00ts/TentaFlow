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

use tentaflow_core::db::repository;
use tentaflow_core::mesh::iroh_manager::{IrohMeshConfig, IrohMeshEvent, IrohMeshManager};
use tentaflow_core::mesh::security::MeshSecurity;
use tentaflow_core::sync::core_capture::CoreWriteCapture;
use tentaflow_core::sync::core_registry::{CORE_SYNC_ADDON_ID, CoreSyncResourceKind};
use tentaflow_core::sync::ledger::{FieldValue, OperationId};
use tentaflow_core::sync::runtime::SqlWriteAction;

const FLOW_ID: &str = "92001";

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
        source.command(&format!(
            "TRUST {} {}",
            receiver.node_id, receiver.public_key
        ));
        receiver.command(&format!("TRUST {} {}", source.node_id, source.public_key));
        source.command(&format!("CONNECT {} {}", receiver.node_id, receiver.addr));
        receiver.command(&format!("CONNECT {} {}", source.node_id, source.addr));
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
    let op_id = op_line
        .split_whitespace()
        .nth(3)
        .expect("record flow op id")
        .to_string();
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

async fn child_main() {
    let home = PathBuf::from(std::env::var("TENTAFLOW_PROCESS_E2E_HOME").expect("child home env"));
    std::fs::create_dir_all(home.join("data")).expect("home data");
    unsafe {
        std::env::set_var("TENTAFLOW_HOME", &home);
    }

    let db = tentaflow_core::db::init(&home.join("data").join("tentaflow.db")).expect("db");
    let cipher = std::sync::Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0x44; 32]));
    let security = std::sync::Arc::new(MeshSecurity::new(db.clone(), cipher).expect("security"));
    let _runtime =
        tentaflow_core::sync::runtime::init(db.clone(), security.clone()).expect("runtime");
    let local_node_id = security.ed25519_public_key_hex();
    let mesh = std::sync::Arc::new(
        IrohMeshManager::new(
            IrohMeshConfig {
                node_id: String::new(),
                bind_addr: "127.0.0.1:0".parse().expect("bind"),
                relay_url: None,
                enable_lan_discovery: false,
                enable_dht_discovery: false,
            },
            security.clone(),
        )
        .await
        .expect("mesh"),
    );
    let _mesh_task = mesh.start();
    let mut events = mesh.subscribe();
    let mesh_for_events = mesh.clone();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(IrohMeshEvent::SyncPushReceived { from_node_id, data }) => {
                    let payload = rkyv::from_bytes::<
                        tentaflow_protocol::mesh::MeshSyncPushPayload,
                        rkyv::rancor::Error,
                    >(&data)
                    .expect("decode sync push");
                    let Some(ack) =
                        tentaflow_core::sync::runtime::handle_push_payload(&from_node_id, payload)
                            .expect("handle push")
                    else {
                        continue;
                    };
                    let ack_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&ack)
                        .map(|bytes| bytes.to_vec())
                        .expect("encode ack");
                    mesh_for_events
                        .send_sync_ack(&from_node_id, &ack_bytes)
                        .await
                        .expect("send ack");
                }
                Ok(IrohMeshEvent::SyncAckReceived { from_node_id, data }) => {
                    let payload = rkyv::from_bytes::<
                        tentaflow_protocol::mesh::MeshSyncAckPayload,
                        rkyv::rancor::Error,
                    >(&data)
                    .expect("decode sync ack");
                    tentaflow_core::sync::runtime::handle_ack_payload(&from_node_id, payload)
                        .expect("handle ack");
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
            security.add_trusted_key(node_id, public_key, "process-e2e")?;
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
        ["SEED_SOURCE", count, rest @ ..] => {
            let count = count.parse::<usize>()?;
            anyhow::ensure!(rest.len() == count * 2, "target tuple count mismatch");
            seed_source(db, rest)?;
            Ok("SEED_SOURCE".to_string())
        }
        ["RECORD_FLOW"] => {
            let result = tentaflow_core::sync::runtime::record_core_capture(core_flow_capture())?
                .expect("runtime initialized");
            Ok(format!("RECORD_FLOW {}", result.op_id.to_hex()))
        }
        ["PUSH", target] => {
            let payload = tentaflow_core::sync::runtime::build_push_payload_for_target(target, 32)?
                .expect("push payload");
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
                .map(|bytes| bytes.to_vec())
                .expect("encode push");
            mesh.send_sync_push(target, &bytes).await?;
            Ok("PUSH".to_string())
        }
        ["WAIT_FLOW"] => {
            wait_for_flow(db).await?;
            Ok("WAIT_FLOW".to_string())
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

fn seed_source(db: &tentaflow_core::db::DbPool, targets: &[&str]) -> anyhow::Result<()> {
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
        repository::grant_sync_explicit_share(
            db,
            "org-default",
            CORE_SYNC_ADDON_ID,
            "core.flow",
            FLOW_ID,
            "node",
            node_id,
            "sync_receive",
            Some(1),
        )?;
    }
    Ok(())
}

fn core_flow_capture() -> CoreWriteCapture {
    let mut fields = BTreeMap::new();
    fields.insert(
        "name".to_string(),
        FieldValue::String("Process Four Node Flow".to_string()),
    );
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
        FLOW_ID,
        SqlWriteAction::Insert,
        fields,
        Some(1),
    )
}

async fn wait_for_flow(db: &tentaflow_core::db::DbPool) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(flow) = repository::get_flow(db, FLOW_ID.parse()?)? {
            if flow.name == "Process Four Node Flow" {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("flow not materialized")
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
