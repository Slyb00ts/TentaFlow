// ===== File: code_studio/mesh_stream.rs — session streams across the mesh =====
//
// A terminal, a command's output and the index progress are all streams that
// are PRODUCED on the owner node and CONSUMED on the node showing the
// dashboard. §12.2 fixes four properties, and each one exists because the
// obvious implementation gets it wrong:
//
//   * backpressure is a CREDIT WINDOW. The producer blocks when the consumer
//     has not taken delivery; it does not buffer without limit. A stream that
//     buffers freely turns a slow reader into an out-of-memory kill on the node
//     that owns everybody's workspaces.
//   * output past the inline budget goes to an ARTIFACT. A build that prints a
//     hundred megabytes must not travel as stream frames; it lands in the CAS
//     and the stream carries the reference.
//   * a reconnect resumes from `after_seq`, out of a bounded replay buffer.
//     Sequence numbers are monotonic per stream, so the consumer deduplicates
//     by comparing, not by remembering what it has seen.
//   * a stream is CLOSED with a stated reason — end of session, assertion no
//     longer valid, trust withdrawn. A stream that simply stops leaves the UI
//     unable to distinguish "finished" from "we lost the node".
//
// When the gap is older than the replay buffer, a stream that can produce a
// full snapshot (a terminal: its VT grid plus a revision) resynchronizes with
// one; a stream that cannot is closed with `gap` rather than silently skipping
// output.
//
// A stream is identified by `(session, stream, consumer node, consumer user)`,
// and the last of those four is not decoration. Sessions are private per person
// with no administrator override (§5.3, §25.4), while a mesh connection only
// ever proves a NODE. Keying on the node alone would mean that anybody holding
// an account on a trusted node could read a colleague's terminal by naming the
// session id. Two people on the same node therefore hold two different streams,
// and neither can pull the other's frames.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;
use tentaflow_protocol::mesh::{
    CodeStudioStreamClose, CodeStudioStreamFrame, CodeStudioStreamOpenRequest,
    CodeStudioStreamPullRequest, MeshCommandResponsePayload, MeshCommandType,
};
use tentaflow_protocol::ProtocolError;
use tokio::sync::Notify;

/// Frames kept for replay after a reconnect. Must be at least `MAX_WINDOW` so
/// evicting the oldest frame can never discard one the consumer has not
/// acknowledged yet.
const REPLAY_FRAMES: usize = 512;

/// Default and maximum send window, in frames.
pub const DEFAULT_WINDOW: u32 = 128;
pub const MAX_WINDOW: u32 = REPLAY_FRAMES as u32;

/// Inline byte budget per stream. Past this, payload bytes are written to the
/// workspace artifact store and the stream carries references instead.
const DEFAULT_INLINE_BUDGET: u64 = 4 * 1024 * 1024;

/// Overflow bytes are flushed to one artifact per chunk, so the buffer held in
/// memory is bounded regardless of how much the process prints.
const OVERFLOW_CHUNK: usize = 512 * 1024;

pub const KIND_DATA: &str = "data";
pub const KIND_SNAPSHOT: &str = "snapshot";
pub const KIND_ARTIFACT: &str = "artifact";

pub const REASON_COMPLETED: &str = "completed";
pub const REASON_SESSION_CLOSED: &str = "session_closed";
pub const REASON_ASSERTION_REVOKED: &str = "assertion_revoked";
pub const REASON_TRUST_LOST: &str = "trust_lost";
pub const REASON_GAP: &str = "gap";
pub const REASON_ERROR: &str = "error";
/// The actor lost the membership, the role or the permission the stream was
/// opened under. Deliberately the same word the local subscription ends with
/// (`stream_handlers::CS_END_PERMISSION_REVOKED`): the consumer forwards the
/// owner's reason to the browser unchanged, so the two paths must not name the
/// same event differently.
pub const REASON_PERMISSION_REVOKED: &str = "permission_revoked";

/// A stream that can restate its whole state instead of replaying deltas.
///
/// The terminal is the reason this exists: its VT grid plus a revision is a
/// complete answer, so a consumer that fell behind the replay buffer gets the
/// screen it should be showing rather than an apology.
pub trait SnapshotSource: Send + Sync {
    /// `(revision, payload)` describing the stream's full current state, or
    /// `None` when the state is gone (the shell exited and its grid was
    /// reaped). `None` is treated as a real gap rather than an empty screen —
    /// a blank terminal is a lie about what the user typed.
    fn snapshot(&self) -> Option<(u64, Vec<u8>)>;
}

/// Identity of one stream. The consumer half of it is what makes a stream
/// private: `pull_for_peer` matches on the whole key, so a peer that names
/// somebody else's session gets the answer it would get for a session that was
/// never opened.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StreamKey {
    /// Empty for the workspace-scoped index stream, whose `stream_id` carries
    /// the workspace instead.
    session_id: String,
    stream_id: String,
    consumer_node_id: String,
    consumer_user_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("stream {session_id}/{stream_id} does not exist")]
    NotFound {
        session_id: String,
        stream_id: String,
    },
    #[error("stream {session_id}/{stream_id} is closed: {reason}")]
    Closed {
        session_id: String,
        stream_id: String,
        reason: String,
    },
    #[error("artifact store: {0}")]
    Artifact(String),
    #[error("owner node '{node_id}' did not serve the stream: {detail}")]
    Transport { node_id: String, detail: String },
    /// The owner node authorized the actor and said no. The typed error travels
    /// whole so the consumer can tell a refusal from a lost node — and so a
    /// refusal for somebody else's session stays indistinguishable from one for
    /// a session that does not exist.
    #[error("owner node refused the stream: {}", .0.message)]
    Denied(ProtocolError),
}

struct Overflow {
    buffer: Vec<u8>,
}

struct StreamInner {
    next_seq: u64,
    /// Highest sequence the consumer has taken delivery of.
    acked: u64,
    ring: VecDeque<CodeStudioStreamFrame>,
    closed: Option<CodeStudioStreamClose>,
    inline_remaining: u64,
    overflow: Option<Overflow>,
}

struct StreamState {
    key: StreamKey,
    workspace_id: String,
    window: u32,
    inner: Mutex<StreamInner>,
    /// Woken when the consumer acknowledges — that is what returns credit.
    ///
    /// `notify_one` rather than `notify_waiters`: it stores a permit when
    /// nobody is parked yet, so an acknowledgement that lands between the
    /// producer releasing the lock and awaiting cannot be missed. A stream has
    /// exactly one producer, so a single stored permit is the whole contract.
    credit: Notify,
    snapshot: Option<Arc<dyn SnapshotSource>>,
}

/// What one pull produced.
#[derive(Debug, Clone)]
pub struct PullResult {
    pub frames: Vec<CodeStudioStreamFrame>,
    pub close: Option<CodeStudioStreamClose>,
    pub highest_seq: u64,
}

/// Process-wide registry of the streams this node produces.
#[derive(Default)]
pub struct StreamHub {
    streams: Mutex<HashMap<StreamKey, Arc<StreamState>>>,
}

static HUB: std::sync::OnceLock<StreamHub> = std::sync::OnceLock::new();

pub fn hub() -> &'static StreamHub {
    HUB.get_or_init(StreamHub::default)
}

/// How a stream is opened.
pub struct StreamOpen {
    pub session_id: String,
    pub stream_id: String,
    pub workspace_id: String,
    /// Node the frames travel to — the node the person is connected to, which
    /// is also the node that issued the assertion authorizing the stream.
    pub consumer_node_id: String,
    /// The person the frames belong to. A stream serves exactly this user, and
    /// only through `consumer_node_id`.
    pub consumer_user_id: String,
    /// Clamped to `MAX_WINDOW`; `0` means the default.
    pub window: u32,
    /// Byte budget carried inline before the output overflows to artifacts.
    /// `0` means the default.
    pub inline_budget: u64,
    pub snapshot: Option<Arc<dyn SnapshotSource>>,
}

/// The producer's end of a stream.
///
/// A producer holds this rather than looking the stream up by name on every
/// frame, which is what lets a reconnect replace the stream underneath it: the
/// handle of the previous generation stops being current, its task notices and
/// stops, and no two producers ever write into the same sequence space.
pub struct StreamHandle {
    state: Arc<StreamState>,
}

impl StreamHandle {
    pub fn session_id(&self) -> &str {
        &self.state.key.session_id
    }

    pub fn stream_id(&self) -> &str {
        &self.state.key.stream_id
    }

    pub fn is_closed(&self) -> bool {
        self.state.inner.lock().closed.is_some()
    }

    /// End this stream with a stated reason. Idempotent — the first reason
    /// stands.
    pub fn close(&self, reason: &str, detail: &str) {
        close_state(&self.state, reason, detail);
    }

    /// Diagnostic: frames currently held for replay.
    pub fn buffered(&self) -> usize {
        self.state.inner.lock().ring.len()
    }

    /// Append one frame, waiting while the send window is full.
    ///
    /// This is the backpressure: the call does not return until the consumer
    /// has acknowledged enough for the frame to fit inside the window. A
    /// producer that cannot wait must not be publishing to a stream.
    pub async fn publish(
        &self,
        kind: &str,
        revision: u64,
        data: Vec<u8>,
    ) -> Result<u64, StreamError> {
        let state = &self.state;

        // Past the inline budget the payload becomes an artifact reference. The
        // decision is taken before waiting for credit, so an overflowing stream
        // does not hold the window hostage with bytes that will never travel.
        let (kind, data) = match self.absorb_overflow(kind, data).await? {
            Some(frame) => frame,
            None => return Ok(state.inner.lock().next_seq),
        };

        loop {
            {
                let mut inner = state.inner.lock();
                if let Some(close) = &inner.closed {
                    return Err(StreamError::Closed {
                        session_id: state.key.session_id.clone(),
                        stream_id: state.key.stream_id.clone(),
                        reason: close.reason.clone(),
                    });
                }
                if inner.next_seq - inner.acked < state.window as u64 {
                    inner.next_seq += 1;
                    let frame = CodeStudioStreamFrame {
                        session_id: state.key.session_id.clone(),
                        stream_id: state.key.stream_id.clone(),
                        seq: inner.next_seq,
                        kind: kind.to_string(),
                        revision,
                        data,
                    };
                    let seq = frame.seq;
                    if inner.ring.len() == REPLAY_FRAMES {
                        inner.ring.pop_front();
                    }
                    inner.ring.push_back(frame);
                    return Ok(seq);
                }
            }
            // Window exhausted — wait for an acknowledgement instead of growing.
            state.credit.notified().await;
        }
    }

    /// Moves payload bytes into the artifact store once the inline budget is
    /// spent. Returns the frame to publish, or `None` when the bytes were
    /// buffered and nothing needs to travel yet.
    async fn absorb_overflow(
        &self,
        kind: &str,
        data: Vec<u8>,
    ) -> Result<Option<(String, Vec<u8>)>, StreamError> {
        if kind != KIND_DATA {
            return Ok(Some((kind.to_string(), data)));
        }
        let flush = {
            let mut inner = self.state.inner.lock();
            if inner.overflow.is_none() && inner.inline_remaining >= data.len() as u64 {
                inner.inline_remaining -= data.len() as u64;
                return Ok(Some((KIND_DATA.to_string(), data)));
            }
            let overflow = inner
                .overflow
                .get_or_insert(Overflow { buffer: Vec::new() });
            overflow.buffer.extend_from_slice(&data);
            if overflow.buffer.len() < OVERFLOW_CHUNK {
                None
            } else {
                Some(std::mem::take(&mut overflow.buffer))
            }
        };
        match flush {
            None => Ok(None),
            Some(bytes) => {
                let reference = store_artifact(&self.state.workspace_id, bytes).await?;
                Ok(Some((KIND_ARTIFACT.to_string(), reference.into_bytes())))
            }
        }
    }
}

/// Marks a stream closed and wakes a producer parked on the window so it sees
/// the close instead of waiting for credit that will never come.
fn close_state(state: &Arc<StreamState>, reason: &str, detail: &str) {
    {
        let mut inner = state.inner.lock();
        if inner.closed.is_none() {
            inner.closed = Some(CodeStudioStreamClose {
                reason: reason.to_string(),
                detail: detail.to_string(),
            });
        }
    }
    state.credit.notify_one();
}

impl StreamHub {
    /// Open (or reopen) a stream. Reopening the same key replaces the previous
    /// state — a consumer that reconnects and asks for a stream that was closed
    /// gets a fresh one rather than an ambiguous half-state.
    pub fn open(&self, open: StreamOpen) -> StreamHandle {
        let key = StreamKey {
            session_id: open.session_id,
            stream_id: open.stream_id,
            consumer_node_id: open.consumer_node_id,
            consumer_user_id: open.consumer_user_id,
        };
        let window = match open.window {
            0 => DEFAULT_WINDOW,
            w => w.min(MAX_WINDOW),
        };
        debug_assert!(window as usize <= REPLAY_FRAMES);
        let state = Arc::new(StreamState {
            key: key.clone(),
            workspace_id: open.workspace_id,
            window,
            inner: Mutex::new(StreamInner {
                next_seq: 0,
                acked: 0,
                ring: VecDeque::with_capacity(64),
                closed: None,
                inline_remaining: match open.inline_budget {
                    0 => DEFAULT_INLINE_BUDGET,
                    b => b,
                },
                overflow: None,
            }),
            credit: Notify::new(),
            snapshot: open.snapshot,
        });
        self.streams.lock().insert(key, Arc::clone(&state));
        StreamHandle { state }
    }

    /// Whether this hub still serves THIS generation of the stream. False once
    /// a reconnect replaced it or the stream was forgotten — that is how a
    /// producer of the previous generation learns to stop.
    pub fn is_current(&self, handle: &StreamHandle) -> bool {
        self.streams
            .lock()
            .get(&handle.state.key)
            .map(|held| Arc::ptr_eq(held, &handle.state))
            .unwrap_or(false)
    }

    /// Drop a stream's buffers once the consumer has had its chance to read the
    /// close record. Only removes the stream while `handle` is still the
    /// current generation, so a reconnect that already replaced it is left
    /// alone.
    pub fn forget(&self, handle: &StreamHandle) {
        let mut streams = self.streams.lock();
        if let Some(held) = streams.get(&handle.state.key) {
            if Arc::ptr_eq(held, &handle.state) {
                streams.remove(&handle.state.key);
            }
        }
    }

    /// Serve a read arriving over the mesh.
    ///
    /// A stream belongs to ONE person reached through ONE node. Anyone else —
    /// another user of the same trusted node included — gets exactly the answer
    /// a stream that was never opened produces, so a member cannot probe for
    /// other people's sessions by naming their ids.
    pub fn pull_for_peer(
        &self,
        peer_node_id: &str,
        peer_user_id: &str,
        session_id: &str,
        stream_id: &str,
        after_seq: u64,
        ack_seq: u64,
        credits: u32,
    ) -> Result<PullResult, StreamError> {
        let key = StreamKey {
            session_id: session_id.to_string(),
            stream_id: stream_id.to_string(),
            consumer_node_id: peer_node_id.to_string(),
            consumer_user_id: peer_user_id.to_string(),
        };
        let state =
            self.streams
                .lock()
                .get(&key)
                .cloned()
                .ok_or_else(|| StreamError::NotFound {
                    session_id: session_id.to_string(),
                    stream_id: stream_id.to_string(),
                })?;
        self.pull(&state, after_seq, ack_seq, credits)
    }

    /// Serve one consumer read: acknowledge, then hand back what follows
    /// `after_seq`, at most `credits` frames.
    fn pull(
        &self,
        state: &Arc<StreamState>,
        after_seq: u64,
        ack_seq: u64,
        credits: u32,
    ) -> Result<PullResult, StreamError> {
        let credits = match credits {
            0 => DEFAULT_WINDOW,
            c => c.min(MAX_WINDOW),
        } as usize;

        let mut released = false;
        let result = {
            let mut inner = state.inner.lock();
            if ack_seq > inner.acked {
                inner.acked = ack_seq.min(inner.next_seq);
                released = true;
            }
            let oldest = inner
                .ring
                .front()
                .map(|f| f.seq)
                .unwrap_or(inner.next_seq + 1);
            let highest_seq = inner.next_seq;

            // The consumer is asking for frames that have already been evicted.
            // A stream that can restate itself does; the rest is a real gap.
            // Saturating: `after_seq` arrives from the network and must not be
            // able to overflow this comparison.
            if after_seq.saturating_add(1) < oldest && inner.closed.is_none() {
                match state.snapshot.as_ref().and_then(|source| source.snapshot()) {
                    Some((revision, payload)) => {
                        inner.next_seq += 1;
                        let frame = CodeStudioStreamFrame {
                            session_id: state.key.session_id.clone(),
                            stream_id: state.key.stream_id.clone(),
                            seq: inner.next_seq,
                            kind: KIND_SNAPSHOT.to_string(),
                            revision,
                            data: payload,
                        };
                        if inner.ring.len() == REPLAY_FRAMES {
                            inner.ring.pop_front();
                        }
                        inner.ring.push_back(frame.clone());
                        let highest_seq = inner.next_seq;
                        PullResult {
                            frames: vec![frame],
                            close: None,
                            highest_seq,
                        }
                    }
                    None => {
                        let close = CodeStudioStreamClose {
                            reason: REASON_GAP.to_string(),
                            detail: format!(
                                "frames after {after_seq} are no longer buffered (oldest {oldest})"
                            ),
                        };
                        inner.closed = Some(close.clone());
                        PullResult {
                            frames: Vec::new(),
                            close: Some(close),
                            highest_seq,
                        }
                    }
                }
            } else {
                let frames: Vec<CodeStudioStreamFrame> = inner
                    .ring
                    .iter()
                    .filter(|f| f.seq > after_seq)
                    .take(credits)
                    .cloned()
                    .collect();
                PullResult {
                    frames,
                    close: inner.closed.clone(),
                    highest_seq,
                }
            }
        };
        if released {
            state.credit.notify_one();
        }
        Ok(result)
    }

    /// Close every stream of a session — the session ended, or the assertion
    /// that authorized it is no longer valid. Covers every consumer of that
    /// session: the same person may be watching from two nodes.
    ///
    /// A session id is required: `close_session("")` would name the index
    /// streams, which have no session, and close everybody's.
    pub fn close_session(&self, session_id: &str, reason: &str, detail: &str) -> usize {
        if session_id.is_empty() {
            return 0;
        }
        self.close_matching(reason, detail, |key| key.session_id == session_id)
    }

    /// Close ONE stream, addressed exactly the way a pull addresses it. This is
    /// what a refusal on the read path uses: the actor whose access ended must
    /// be told why, and nobody else's stream may be touched by it.
    pub fn close_for_peer(
        &self,
        peer_node_id: &str,
        peer_user_id: &str,
        session_id: &str,
        stream_id: &str,
        reason: &str,
        detail: &str,
    ) -> bool {
        let key = StreamKey {
            session_id: session_id.to_string(),
            stream_id: stream_id.to_string(),
            consumer_node_id: peer_node_id.to_string(),
            consumer_user_id: peer_user_id.to_string(),
        };
        let state = self.streams.lock().get(&key).cloned();
        match state {
            Some(state) => {
                close_state(&state, reason, detail);
                true
            }
            None => false,
        }
    }

    /// Close every stream flowing to a node we no longer trust. The frames stop
    /// at the same moment the trust does, and the consumer is told why.
    pub fn close_for_node(&self, node_id: &str, reason: &str, detail: &str) -> usize {
        self.close_matching(reason, detail, |key| key.consumer_node_id == node_id)
    }

    fn close_matching(
        &self,
        reason: &str,
        detail: &str,
        matches: impl Fn(&StreamKey) -> bool,
    ) -> usize {
        let states: Vec<Arc<StreamState>> = self
            .streams
            .lock()
            .iter()
            .filter(|(key, _)| matches(key))
            .map(|(_, state)| Arc::clone(state))
            .collect();
        for state in &states {
            close_state(state, reason, detail);
        }
        states.len()
    }
}

/// Writes one overflow chunk into the workspace's artifact store and returns
/// its reference. The blob write is a SQLite transaction, so it runs off the
/// async runtime's worker.
async fn store_artifact(workspace_id: &str, bytes: Vec<u8>) -> Result<String, StreamError> {
    let workspace_id = workspace_id.to_string();
    tokio::task::spawn_blocking(move || {
        let pool = super::workspace_db::open(&workspace_id)
            .map_err(|e| StreamError::Artifact(e.to_string()))?;
        super::artifacts::put(&pool, &workspace_id, &bytes, "stream_overflow")
            .map(|reference| reference.sha256)
            .map_err(|e| StreamError::Artifact(e.to_string()))
    })
    .await
    .map_err(|e| StreamError::Artifact(e.to_string()))?
}

// =============================================================================
// Consumer side
// =============================================================================

/// Consumer-side position in a remote stream.
///
/// Deduplication is a comparison, not a set: sequences are monotonic, so a
/// frame that is not strictly newer than the last one accepted is a repeat and
/// is dropped. A reconnect therefore costs at most one overlapping batch.
#[derive(Debug, Default, Clone)]
pub struct StreamCursor {
    pub last_seq: u64,
    pub acked_seq: u64,
}

/// What the consumer should do with a batch.
#[derive(Debug, Clone)]
pub struct ConsumedBatch {
    /// Frames not seen before, in order.
    pub frames: Vec<CodeStudioStreamFrame>,
    /// Frames dropped as repeats — diagnostics for the reconnect path.
    pub duplicates: usize,
    pub close: Option<CodeStudioStreamClose>,
}

impl StreamCursor {
    /// Where the next pull resumes from.
    pub fn after_seq(&self) -> u64 {
        self.last_seq
    }

    /// Filter one pull result through the cursor and advance it.
    ///
    /// A `snapshot` frame is a restart of the stream's state: everything before
    /// it is superseded, so the cursor jumps to it instead of complaining about
    /// the frames it never saw.
    pub fn accept(&mut self, result: PullResult) -> ConsumedBatch {
        let mut frames = Vec::with_capacity(result.frames.len());
        let mut duplicates = 0usize;
        for frame in result.frames {
            if frame.kind == KIND_SNAPSHOT {
                self.last_seq = frame.seq;
                frames.push(frame);
                continue;
            }
            if frame.seq <= self.last_seq {
                duplicates += 1;
                continue;
            }
            self.last_seq = frame.seq;
            frames.push(frame);
        }
        // Everything handed to the caller is delivered, so it is acknowledged.
        self.acked_seq = self.last_seq;
        ConsumedBatch {
            frames,
            duplicates,
            close: result.close,
        }
    }
}

/// One remote stream as the dashboard node consumes it.
///
/// Every call it makes carries a freshly minted `SessionAssertion` (§12.1): the
/// mesh connection proves the node, and the owner node needs to know WHO is
/// reading before it hands over somebody's terminal. Minting per call is also
/// what bounds a revocation — an assertion lives at most 120 s, and the owner
/// re-authorizes on every one of them.
pub struct RemoteStream {
    pub iroh: Arc<crate::mesh::iroh_manager::IrohMeshManager>,
    pub owner_node_id: String,
    pub workspace_id: String,
    /// Empty for the workspace-scoped index stream.
    pub session_id: String,
    pub stream_id: String,
    /// This node's main database — where the actor's role and capabilities are
    /// resolved before every assertion is minted.
    pub db: crate::db::DbPool,
    pub local_node_id: String,
    pub user_id: String,
    pub org_id: String,
    pub cursor: StreamCursor,
}

impl RemoteStream {
    /// Ask the owner node to open the stream for this actor.
    ///
    /// `after_revision` is the PRODUCER's resume point (event `seq`, VT grid
    /// revision, index progress `seq`), not a hub sequence: the hub's sequence
    /// space belongs to one connection and restarts with it.
    pub async fn open(
        &self,
        after_revision: u64,
        window: u32,
        timeout_secs: u64,
    ) -> Result<u64, StreamError> {
        let request = CodeStudioStreamOpenRequest {
            workspace_id: self.workspace_id.clone(),
            session_id: self.session_id.clone(),
            stream_id: self.stream_id.clone(),
            after_revision,
            window,
        };
        let request_cbor = self.encode(&request)?;
        let assertion = self.mint(&request_cbor)?;
        let response = self
            .send(
                MeshCommandType::CodeStudioStreamOpen {
                    assertion,
                    request_cbor,
                },
                timeout_secs,
            )
            .await?;
        let (payload, transport_error) = (response.payload, response.error);
        match payload {
            MeshCommandResponsePayload::CodeStudioStreamOpenResult { highest_seq, error } => {
                match error {
                    Some(error) => Err(StreamError::Denied(error)),
                    None => Ok(highest_seq),
                }
            }
            _ => Err(self.transport(transport_error)),
        }
    }

    /// Read the next batch. One round trip carries the acknowledgement that
    /// returns credit to the producer, the resume point, and the window this
    /// node is willing to take. Everything the cursor hands back is new —
    /// repeats from an overlapping resume are dropped here, not by the caller.
    pub async fn pull(
        &mut self,
        credits: u32,
        timeout_secs: u64,
    ) -> Result<ConsumedBatch, StreamError> {
        let request = CodeStudioStreamPullRequest {
            session_id: self.session_id.clone(),
            stream_id: self.stream_id.clone(),
            after_seq: self.cursor.after_seq(),
            ack_seq: self.cursor.acked_seq,
            credits,
        };
        let request_cbor = self.encode(&request)?;
        let assertion = self.mint(&request_cbor)?;
        let response = self
            .send(
                MeshCommandType::CodeStudioStreamPull {
                    assertion,
                    request_cbor,
                },
                timeout_secs,
            )
            .await?;
        let (payload, transport_error) = (response.payload, response.error);
        match payload {
            MeshCommandResponsePayload::CodeStudioStreamResult {
                frames,
                close,
                highest_seq,
            } => Ok(self.cursor.accept(PullResult {
                frames,
                close,
                highest_seq,
            })),
            _ => Err(self.transport(transport_error)),
        }
    }

    fn encode<T: serde::Serialize>(&self, request: &T) -> Result<Vec<u8>, StreamError> {
        crate::mesh::cbor::encode(request).map_err(|e| StreamError::Transport {
            node_id: self.owner_node_id.clone(),
            detail: format!("stream request encode failed: {e}"),
        })
    }

    /// Mint the assertion binding these exact request bytes to this actor. The
    /// capabilities and the RBAC revision are resolved from THIS node's live
    /// database, so a role taken away here is gone from the next call.
    fn mint(
        &self,
        request_cbor: &[u8],
    ) -> Result<tentaflow_protocol::mesh::SessionAssertion, StreamError> {
        super::remote_proxy::mint_stream_assertion(
            &self.db,
            &self.local_node_id,
            &self.owner_node_id,
            &self.user_id,
            &self.org_id,
            &self.workspace_id,
            &self.session_id,
            request_cbor,
        )
        .map_err(StreamError::Denied)
    }

    async fn send(
        &self,
        command: MeshCommandType,
        timeout_secs: u64,
    ) -> Result<crate::mesh::iroh_manager::CommandWaitResponse, StreamError> {
        self.iroh
            .send_command_and_wait(&self.owner_node_id, command, timeout_secs)
            .await
            .map_err(|e| StreamError::Transport {
                node_id: self.owner_node_id.clone(),
                detail: e.to_string(),
            })
    }

    fn transport(&self, detail: Option<String>) -> StreamError {
        StreamError::Transport {
            node_id: self.owner_node_id.clone(),
            detail: detail.unwrap_or_else(|| "unexpected mesh response".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    const NODE: &str = "node-a";
    const ALICE: &str = "user-alice";
    const BOB: &str = "user-bob";

    struct GridSnapshot {
        revision: AtomicU64,
    }

    impl SnapshotSource for GridSnapshot {
        fn snapshot(&self) -> Option<(u64, Vec<u8>)> {
            Some((self.revision.load(Ordering::Relaxed), b"full-grid".to_vec()))
        }
    }

    fn open_for(
        hub: &StreamHub,
        session: &str,
        user: &str,
        window: u32,
        snapshot: bool,
    ) -> StreamHandle {
        hub.open(StreamOpen {
            session_id: session.to_string(),
            stream_id: "s".to_string(),
            workspace_id: format!("ws-{session}"),
            consumer_node_id: NODE.to_string(),
            consumer_user_id: user.to_string(),
            window,
            inline_budget: 0,
            snapshot: snapshot.then(|| {
                Arc::new(GridSnapshot {
                    revision: AtomicU64::new(7),
                }) as Arc<dyn SnapshotSource>
            }),
        })
    }

    fn open_stream(hub: &StreamHub, session: &str, window: u32, snapshot: bool) -> StreamHandle {
        open_for(hub, session, ALICE, window, snapshot)
    }

    fn pull(
        hub: &StreamHub,
        user: &str,
        session: &str,
        after_seq: u64,
        ack_seq: u64,
        credits: u32,
    ) -> Result<PullResult, StreamError> {
        hub.pull_for_peer(NODE, user, session, "s", after_seq, ack_seq, credits)
    }

    #[tokio::test]
    async fn reconnect_resumes_without_a_gap_and_without_a_duplicate() {
        let hub = StreamHub::default();
        let handle = open_stream(&hub, "sess-reconnect", 64, false);
        for i in 0..10u8 {
            handle
                .publish(KIND_DATA, 0, vec![i])
                .await
                .expect("publish");
        }

        let mut cursor = StreamCursor::default();
        let first = pull(
            &hub,
            ALICE,
            "sess-reconnect",
            cursor.after_seq(),
            cursor.acked_seq,
            5,
        )
        .expect("pull");
        let batch = cursor.accept(first);
        assert_eq!(batch.frames.len(), 5);
        assert_eq!(cursor.last_seq, 5);

        // The connection drops and comes back asking from an OLDER point than
        // the consumer actually reached — the classic source of duplicates.
        let resumed = pull(&hub, ALICE, "sess-reconnect", 3, cursor.acked_seq, 64).expect("pull");
        let batch = cursor.accept(resumed);
        assert_eq!(batch.duplicates, 2, "frames 4 and 5 were already delivered");
        let seqs: Vec<u64> = batch.frames.iter().map(|f| f.seq).collect();
        assert_eq!(seqs, vec![6, 7, 8, 9, 10], "no gap, no repeat");
        assert_eq!(cursor.last_seq, 10);
    }

    #[tokio::test]
    async fn an_exhausted_window_blocks_the_producer_instead_of_buffering() {
        let hub = Arc::new(StreamHub::default());
        let handle = Arc::new(open_stream(&hub, "sess-window", 4, false));
        for i in 0..4u8 {
            handle
                .publish(KIND_DATA, 0, vec![i])
                .await
                .expect("publish");
        }
        assert_eq!(handle.buffered(), 4);

        let blocked = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.publish(KIND_DATA, 0, vec![99]).await })
        };
        // The window is full: the fifth publish must not complete, and the
        // buffer must not grow behind our back.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!blocked.is_finished(), "the producer must wait for credit");
        assert_eq!(
            handle.buffered(),
            4,
            "a blocked producer must not have enqueued anything"
        );

        // Acknowledging returns credit and the producer resumes.
        pull(&hub, ALICE, "sess-window", 0, 4, 64).expect("pull");
        let seq = tokio::time::timeout(Duration::from_secs(2), blocked)
            .await
            .expect("the producer must be released by the ack")
            .expect("task")
            .expect("publish");
        assert_eq!(seq, 5);
    }

    /// A consumer that comes back asking for frames older than the replay
    /// buffer still holds gets the whole grid plus its revision, not a hole.
    #[tokio::test]
    async fn a_gap_past_the_replay_buffer_resynchronizes_a_terminal() {
        let hub = StreamHub::default();
        let handle = open_stream(&hub, "sess-snapshot", 8, true);
        for i in 0..(REPLAY_FRAMES + 5) {
            handle
                .publish(KIND_DATA, i as u64, vec![0])
                .await
                .expect("publish");
            pull(&hub, ALICE, "sess-snapshot", i as u64, i as u64 + 1, 64).expect("ack");
        }
        assert_eq!(handle.buffered(), REPLAY_FRAMES);

        let mut cursor = StreamCursor {
            last_seq: 1,
            acked_seq: 1,
        };
        let result = pull(
            &hub,
            ALICE,
            "sess-snapshot",
            cursor.after_seq(),
            cursor.acked_seq,
            64,
        )
        .expect("pull");
        assert!(
            result.close.is_none(),
            "a terminal resyncs, it does not close"
        );
        let batch = cursor.accept(result);
        assert_eq!(batch.frames.len(), 1);
        assert_eq!(batch.frames[0].kind, KIND_SNAPSHOT);
        assert_eq!(batch.frames[0].revision, 7);
        assert!(cursor.last_seq > 1, "the cursor jumps to the snapshot");
    }

    #[tokio::test]
    async fn a_gap_on_a_stream_without_a_snapshot_closes_with_a_reason() {
        let hub = StreamHub::default();
        let handle = open_stream(&hub, "sess-gap", 8, false);
        for i in 0..(REPLAY_FRAMES + 5) {
            handle
                .publish(KIND_DATA, i as u64, vec![0])
                .await
                .expect("publish");
            pull(&hub, ALICE, "sess-gap", i as u64, i as u64 + 1, 64).expect("ack");
        }
        let result = pull(&hub, ALICE, "sess-gap", 1, 1, 64).expect("pull");
        assert!(result.frames.is_empty());
        let close = result.close.expect("a gap must close the stream");
        assert_eq!(close.reason, REASON_GAP);
    }

    #[tokio::test]
    async fn losing_trust_closes_the_streams_going_to_that_node() {
        let hub = StreamHub::default();
        let handle = open_stream(&hub, "sess-trust", 8, false);
        handle
            .publish(KIND_DATA, 0, vec![1])
            .await
            .expect("publish");

        assert_eq!(hub.close_for_node(NODE, REASON_TRUST_LOST, "revoked"), 1);
        let result = pull(&hub, ALICE, "sess-trust", 0, 0, 64).expect("pull");
        assert_eq!(result.close.expect("closed").reason, REASON_TRUST_LOST);
        assert!(matches!(
            handle.publish(KIND_DATA, 0, vec![2]).await,
            Err(StreamError::Closed { .. })
        ));
    }

    #[tokio::test]
    async fn closing_a_session_states_the_reason_once() {
        let hub = StreamHub::default();
        let handle = open_stream(&hub, "sess-close", 8, false);
        assert_eq!(
            hub.close_session("sess-close", REASON_SESSION_CLOSED, ""),
            1
        );
        handle.close(REASON_ERROR, "later");
        let result = pull(&hub, ALICE, "sess-close", 0, 0, 64).expect("pull");
        assert_eq!(result.close.expect("closed").reason, REASON_SESSION_CLOSED);
    }

    /// The hole this closes: the hub used to bind a stream to a NODE, so any
    /// account on a trusted node could read a colleague's session by naming its
    /// id. Two people on ONE node now hold two streams, and neither pull
    /// reaches the other's frames.
    #[tokio::test]
    async fn two_users_on_one_node_hold_two_independent_streams() {
        let hub = StreamHub::default();
        let alice = open_for(&hub, "sess-alice", ALICE, 8, false);
        let bob = open_for(&hub, "sess-bob", BOB, 8, false);
        alice
            .publish(KIND_DATA, 0, b"alice".to_vec())
            .await
            .expect("publish");
        bob.publish(KIND_DATA, 0, b"bob".to_vec())
            .await
            .expect("publish");

        let mine = pull(&hub, ALICE, "sess-alice", 0, 0, 64).expect("pull");
        assert_eq!(mine.frames.len(), 1);
        assert_eq!(mine.frames[0].data, b"alice");

        let theirs = pull(&hub, BOB, "sess-bob", 0, 0, 64).expect("pull");
        assert_eq!(theirs.frames.len(), 1);
        assert_eq!(theirs.frames[0].data, b"bob");

        // Bob names Alice's session — the id is not a secret, the binding is.
        let probe = pull(&hub, BOB, "sess-alice", 0, 0, 64);
        assert!(matches!(probe, Err(StreamError::NotFound { .. })));

        // And the answer is WORD FOR WORD the one an unopened session gives, so
        // the refusal cannot be used to tell the two apart.
        let unknown = pull(&hub, BOB, "sess-does-not-exist", 0, 0, 64);
        assert_eq!(
            probe.unwrap_err().to_string().replace("sess-alice", "X"),
            unknown
                .unwrap_err()
                .to_string()
                .replace("sess-does-not-exist", "X")
        );
    }

    /// A stream is bound to the node as well: the same person reaching us from
    /// a node that never opened the stream is not the consumer it was opened
    /// for.
    #[tokio::test]
    async fn a_stream_is_not_served_to_another_node() {
        let hub = StreamHub::default();
        let handle = open_stream(&hub, "sess-node", 8, false);
        handle
            .publish(KIND_DATA, 0, vec![1])
            .await
            .expect("publish");
        let other = hub.pull_for_peer("node-b", ALICE, "sess-node", "s", 0, 0, 64);
        assert!(matches!(other, Err(StreamError::NotFound { .. })));
    }

    /// A reconnect opens the stream again. The producer of the previous
    /// generation has to notice and stop, or two producers would write into one
    /// sequence space.
    #[tokio::test]
    async fn reopening_retires_the_previous_generation() {
        let hub = StreamHub::default();
        let first = open_stream(&hub, "sess-regen", 8, false);
        assert!(hub.is_current(&first));
        let second = open_stream(&hub, "sess-regen", 8, false);
        assert!(!hub.is_current(&first), "the old handle must retire");
        assert!(hub.is_current(&second));

        // Forgetting through the retired handle must not remove the live
        // stream that replaced it.
        hub.forget(&first);
        assert!(hub.is_current(&second));
        hub.forget(&second);
        assert!(!hub.is_current(&second));
    }

    #[test]
    fn a_snapshot_frame_moves_the_cursor_forward_wholesale() {
        let mut cursor = StreamCursor {
            last_seq: 3,
            acked_seq: 3,
        };
        let batch = cursor.accept(PullResult {
            frames: vec![CodeStudioStreamFrame {
                session_id: "s".into(),
                stream_id: "t".into(),
                seq: 900,
                kind: KIND_SNAPSHOT.into(),
                revision: 42,
                data: Vec::new(),
            }],
            close: None,
            highest_seq: 900,
        });
        assert_eq!(batch.frames.len(), 1);
        assert_eq!(cursor.last_seq, 900);
    }
}
