// =============================================================================
// Plik: deploy/log_bus.rs
// Opis: Globalna szyna broadcastów dla live logów deploymentu. Runner pisze
//       kolejne LogLine per deploy_id, streaming handler (DeploymentLogStream)
//       subscribes i re-emituje jako MessageBody::DeploymentBody(StreamChunk).
//       Każdy deploy_id ma swój tokio::sync::broadcast channel tworzony lazy
//       przy pierwszym wpisaniu i kasowany gdy runner kończy (StreamEnd).
// =============================================================================

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct LogLine {
    pub deploy_id: String,
    /// "log" = linia build/run output, "phase" = zmiana fazy, "progress" = %.
    pub kind: String,
    pub line: String,
    pub phase: String,
    pub progress_pct: u32,
    pub ts_ms: i64,
}

#[derive(Clone, Debug)]
pub enum BusMessage {
    Line(LogLine),
    End {
        deploy_id: String,
        final_status: String,
        image_tag: String,
        container_name: String,
        error_message: String,
        duration_ms: i64,
    },
}

struct Bus {
    channels: RwLock<HashMap<String, broadcast::Sender<BusMessage>>>,
}

static BUS: OnceLock<Arc<Bus>> = OnceLock::new();

fn bus() -> &'static Arc<Bus> {
    BUS.get_or_init(|| {
        Arc::new(Bus {
            channels: RwLock::new(HashMap::new()),
        })
    })
}

/// Gwarantuje że broadcast channel istnieje dla deploy_id i zwraca nadawcę.
pub fn sender_for(deploy_id: &str) -> broadcast::Sender<BusMessage> {
    {
        let map = bus().channels.read();
        if let Some(s) = map.get(deploy_id) {
            return s.clone();
        }
    }
    let mut map = bus().channels.write();
    map.entry(deploy_id.to_string())
        .or_insert_with(|| broadcast::channel::<BusMessage>(1024).0)
        .clone()
}

/// Subscribe do logów dla istniejącego deploy_id. None = kanał nie istnieje
/// (deploy już zakończony i kasowany), caller powinien polegać na replay_tail.
pub fn subscribe(deploy_id: &str) -> Option<broadcast::Receiver<BusMessage>> {
    let map = bus().channels.read();
    map.get(deploy_id).map(|s| s.subscribe())
}

/// Usuwa kanał po StreamEnd. Subscriberzy dostaną broadcast::RecvError::Closed.
pub fn close(deploy_id: &str) {
    let mut map = bus().channels.write();
    map.remove(deploy_id);
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ---------------------------------------------------------------------------
// Emit helpers — shared between deploy::runner and deploy::redeploy. Same
// semantics: append to deployments.log_tail AND broadcast to live subscribers.
// ---------------------------------------------------------------------------

use crate::db::repository::deployments as deployments_repo;
use crate::db::DbPool;

pub fn log_line(
    db: &DbPool,
    deploy_id: &str,
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    kind: &str,
    line: &str,
) {
    let _ = deployments_repo::append_log_line(db, deploy_id, line);
    let _ = tx.send(BusMessage::Line(LogLine {
        deploy_id: deploy_id.to_string(),
        kind: kind.to_string(),
        line: line.to_string(),
        phase: String::new(),
        progress_pct: 0,
        ts_ms: now_ms(),
    }));
}

pub fn progress(
    db: &DbPool,
    deploy_id: &str,
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    pct: u32,
) {
    let _ = deployments_repo::set_status(db, deploy_id, "building", "building", pct);
    let _ = tx.send(BusMessage::Line(LogLine {
        deploy_id: deploy_id.to_string(),
        kind: "progress".to_string(),
        line: String::new(),
        phase: "building".to_string(),
        progress_pct: pct,
        ts_ms: now_ms(),
    }));
}

pub fn phase(
    db: &DbPool,
    deploy_id: &str,
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    status: &str,
    pct: u32,
    phase_name: &str,
) {
    let _ = deployments_repo::set_status(db, deploy_id, status, phase_name, pct);
    let _ = tx.send(BusMessage::Line(LogLine {
        deploy_id: deploy_id.to_string(),
        kind: "phase".to_string(),
        line: phase_name.to_string(),
        phase: phase_name.to_string(),
        progress_pct: pct,
        ts_ms: now_ms(),
    }));
}

pub async fn finish_success(
    db: &DbPool,
    deploy_id: &str,
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    start_ms: i64,
    image_tag: String,
    container_name: String,
) {
    let _ = deployments_repo::mark_finished(db, deploy_id, "success", None);
    let _ = tx.send(BusMessage::End {
        deploy_id: deploy_id.to_string(),
        final_status: "success".to_string(),
        image_tag,
        container_name,
        error_message: String::new(),
        duration_ms: now_ms() - start_ms,
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    close(deploy_id);
}

pub async fn fail(
    db: &DbPool,
    deploy_id: &str,
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    start_ms: i64,
    msg: &str,
) {
    let _ = deployments_repo::append_log_line(db, deploy_id, &format!("[error] {}", msg));
    let _ = deployments_repo::mark_finished(db, deploy_id, "failure", Some(msg));
    let _ = tx.send(BusMessage::End {
        deploy_id: deploy_id.to_string(),
        final_status: "failure".to_string(),
        image_tag: String::new(),
        container_name: String::new(),
        error_message: msg.to_string(),
        duration_ms: now_ms() - start_ms,
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    close(deploy_id);
}
