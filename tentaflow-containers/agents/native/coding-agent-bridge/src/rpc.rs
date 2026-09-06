// ============ File: rpc.rs — Bounded JSON-RPC transport for supervised provider processes. ============

use crate::{cli_command, process, spawn_cli};
use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex as SyncMutex;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    process::Child,
    sync::{mpsc, oneshot, Mutex},
};

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

pub(crate) enum Inbound {
    Frame(Value),
    Closed(String),
}

struct Shared {
    stdin: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    pending: SyncMutex<HashMap<u64, oneshot::Sender<Value>>>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

struct PendingRequest {
    shared: Arc<Shared>,
    id: u64,
    written: bool,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        let mut pending = self.shared.pending.lock();
        pending.remove(&self.id);
        if !self.written {
            // A cancelled write may leave half a frame in the pipe.
            self.shared.closed.store(true, Ordering::Release);
            pending.clear();
        }
    }
}

#[derive(Clone)]
pub(crate) struct RpcClient(Arc<Shared>);

pub(crate) struct JsonRpc {
    client: RpcClient,
    handle: process::Handle,
    _child: Child,
}

impl JsonRpc {
    pub(crate) fn spawn(
        argv: Vec<String>,
        workspace: &str,
        env: &[(String, String)],
        processes: &process::Registry,
        kind: &str,
    ) -> Result<(Self, mpsc::Receiver<Inbound>)> {
        let (mut command, supervisor_root) = cli_command(argv, Path::new(workspace), env)?;
        command
            .current_dir(workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = spawn_cli(&mut command)?;
        let handle = processes.track(
            kind,
            child.id().context("provider has no pid")?,
            supervisor_root,
        )?;
        let stdin = child.stdin.take().context("provider stdin missing")?;
        let stdout = child.stdout.take().context("provider stdout missing")?;
        let (client, inbound) = connect(stdout, stdin);
        Ok((
            Self {
                client,
                handle,
                _child: child,
            },
            inbound,
        ))
    }

    pub(crate) fn client(&self) -> RpcClient {
        self.client.clone()
    }

    pub(crate) async fn shutdown(&mut self) -> process::ProcessState {
        let result = self.handle.terminate();
        if result != process::ProcessState::Running {
            self.client.0.closed.store(true, Ordering::Release);
            self.client.0.pending.lock().clear();
        }
        result
    }
}

impl RpcClient {
    pub(crate) async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        if self.0.closed.load(Ordering::Acquire) {
            return Err(anyhow!("provider connection is closed"));
        }
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.0.pending.lock().insert(id, sender);
        let mut pending = PendingRequest {
            shared: self.0.clone(),
            id,
            written: false,
        };
        let response = tokio::time::timeout(timeout, async {
            self.write(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                .await?;
            pending.written = true;
            receiver
                .await
                .context("provider connection closed before response")
        })
        .await
        .context("provider RPC timed out")??;
        if let Some(error) = response.get("error") {
            return Err(anyhow!("provider {method}: {error}"));
        }
        Ok(response)
    }

    pub(crate) async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }

    pub(crate) async fn write(&self, mut value: Value) -> Result<()> {
        if self.0.closed.load(Ordering::Acquire) {
            return Err(anyhow!("provider connection is closed"));
        }
        let object = value
            .as_object_mut()
            .context("RPC frame must be an object")?;
        object.entry("jsonrpc").or_insert(json!("2.0"));
        let mut bytes = serde_json::to_vec(&value)?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(anyhow!("provider RPC frame exceeds limit"));
        }
        bytes.push(b'\n');
        let mut stdin = self.0.stdin.lock().await;
        stdin.write_all(&bytes).await?;
        stdin.flush().await?;
        Ok(())
    }
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut BufReader<R>) -> Result<Option<Value>> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if frame.is_empty() {
                return Ok(None);
            }
            return Err(anyhow!("provider closed an incomplete RPC frame"));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map(|index| index + 1).unwrap_or(available.len());
        if frame.len() + count > MAX_FRAME_BYTES {
            return Err(anyhow!("provider RPC frame exceeds limit"));
        }
        frame.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            if frame.iter().all(u8::is_ascii_whitespace) {
                frame.clear();
                continue;
            }
            return Ok(Some(
                serde_json::from_slice(&frame).context("invalid provider JSON-RPC frame")?,
            ));
        }
    }
}

fn connect<R, W>(stdout: R, stdin: W) -> (RpcClient, mpsc::Receiver<Inbound>)
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let shared = Arc::new(Shared {
        stdin: Mutex::new(Box::new(stdin)),
        pending: SyncMutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        closed: AtomicBool::new(false),
    });
    let client = RpcClient(shared.clone());
    let (sender, receiver) = mpsc::channel(64);
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let reason = loop {
            match read_frame(&mut reader).await {
                Ok(Some(value)) => {
                    if value.get("method").is_none() {
                        if let Some(id) = value.get("id").and_then(Value::as_u64) {
                            if let Some(pending) = shared.pending.lock().remove(&id) {
                                let _ = pending.send(value);
                            }
                            continue;
                        }
                    }
                    if sender.send(Inbound::Frame(value)).await.is_err() {
                        break "provider event reader stopped".into();
                    }
                }
                Ok(None) => break "provider closed its output".into(),
                Err(error) => break error.to_string(),
            }
        };
        shared.closed.store(true, Ordering::Release);
        shared.pending.lock().clear();
        let _ = sender.send(Inbound::Closed(reason)).await;
    });
    (client, receiver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn responses_and_reverse_requests_are_routed_separately() {
        let (local, remote) = tokio::io::duplex(4096);
        let (local_read, local_write) = tokio::io::split(local);
        let (client, mut inbound) = connect(local_read, local_write);
        tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(remote);
            let request = read_frame(&mut BufReader::new(read))
                .await
                .unwrap()
                .unwrap();
            write.write_all(format!("{}\n{}\n", json!({"id":"approval-1","method":"session/request_permission","params":{}}), json!({"id":request["id"],"result":{"ok":true}})).as_bytes()).await.unwrap();
        });
        let response = client
            .request("initialize", json!({}), Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(response["result"]["ok"], true);
        let Some(Inbound::Frame(request)) = inbound.recv().await else {
            panic!("reverse request missing");
        };
        assert_eq!(request["id"], "approval-1");
        assert!(client.0.pending.lock().is_empty());
    }

    #[tokio::test]
    async fn timed_out_requests_are_removed() {
        let (local, _remote) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(local);
        let (client, _inbound) = connect(read, write);
        assert!(client
            .request("never", json!({}), Duration::from_millis(1))
            .await
            .is_err());
        assert!(client.0.pending.lock().is_empty());
    }

    #[tokio::test]
    async fn blocked_writes_time_out_and_close_partial_frames() {
        let (local, _remote) = tokio::io::duplex(32);
        let (read, write) = tokio::io::split(local);
        let (client, _inbound) = connect(read, write);
        assert!(client
            .request(
                "blocked",
                json!({"text":"x".repeat(1024)}),
                Duration::from_millis(10)
            )
            .await
            .is_err());
        assert!(client.0.pending.lock().is_empty());
        assert!(client.0.closed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn cancelled_requests_release_pending_entries() {
        let (local, mut remote) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(local);
        let (client, _inbound) = connect(read, write);
        let requester = client.clone();
        let task = tokio::spawn(async move {
            requester
                .request("cancel", json!({}), Duration::from_secs(60))
                .await
        });
        let mut request = String::new();
        BufReader::new(&mut remote)
            .read_line(&mut request)
            .await
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(client.0.pending.lock().is_empty());
    }

    #[tokio::test]
    async fn invalid_and_oversized_frames_close_the_transport() {
        assert!(read_frame(&mut BufReader::new(&b"invalid\n"[..]))
            .await
            .is_err());
        let oversized = vec![b'x'; MAX_FRAME_BYTES + 1];
        assert!(read_frame(&mut BufReader::new(&oversized[..]))
            .await
            .is_err());
    }
}
