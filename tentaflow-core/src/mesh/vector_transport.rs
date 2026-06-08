// ============ File: mesh/vector_transport.rs — RemoteVectorTransport over the mesh ============
//
// Bridges the vector module's `RemoteVectorTransport` seam to the mesh: it ships
// an opaque VectorOp request CBOR to the owning node via `IrohMeshManager`'s
// command channel and returns the response CBOR. This is the ONLY place that
// knows both the iroh mesh handle and the `MeshCommandType::VectorOp` wire
// variant — the vector module stays free of mesh/protocol deps. Mirrors the
// async→sync bridge used by the web-research mesh proxy.

use std::sync::Arc;

use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType};

use crate::mesh::iroh_manager::IrohMeshManager;
use crate::services::vector::error::{Result, VectorError};
use crate::services::vector::RemoteVectorTransport;

/// Wraps the iroh mesh manager as the transport for remote Milvus backends.
pub struct MeshVectorTransport {
    mesh: Arc<IrohMeshManager>,
    /// Runtime handle captured at construction (router init runs inside the
    /// app's multi-threaded runtime). Stored so `execute` can drive the async
    /// mesh call even from a thread with no current runtime (e.g. a
    /// `spawn_blocking` worker), without relying on `Handle::current()`.
    handle: tokio::runtime::Handle,
}

impl MeshVectorTransport {
    /// Must be called from within the app's tokio runtime (it is — router init).
    /// Panicking here at startup is preferable to a latent per-op panic later.
    pub fn new(mesh: Arc<IrohMeshManager>) -> Self {
        Self {
            mesh,
            handle: tokio::runtime::Handle::current(),
        }
    }
}

impl RemoteVectorTransport for MeshVectorTransport {
    fn execute(&self, node_id: &str, request_cbor: Vec<u8>) -> Result<Vec<u8>> {
        let mesh = self.mesh.clone();
        let node = node_id.to_string();
        let fut = mesh.send_command(&node, MeshCommandType::VectorOp { request_cbor });
        // The VectorBackend trait is synchronous; bridge to the async mesh call.
        // On a runtime worker thread we MUST yield it via `block_in_place` (else
        // we'd block a worker); off-runtime (spawn_blocking / plain thread) we
        // drive the future directly on the captured handle. Either way: no
        // `Handle::current()`, so no panic when no runtime is in scope.
        let response = if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| self.handle.block_on(fut))
        } else {
            self.handle.block_on(fut)
        }
        .map_err(|e| VectorError::Backend(format!("mesh vector op ({node_id}): {e}")))?;
        if !response.ok {
            return Err(VectorError::Backend(response.error.unwrap_or_else(|| {
                format!("remote vector op failed on node {node_id}")
            })));
        }
        match response.payload {
            MeshCommandResponsePayload::VectorOpResult { result_cbor } => Ok(result_cbor),
            _ => Err(VectorError::Backend(format!(
                "remote vector op on node {node_id} returned unexpected payload"
            ))),
        }
    }
}
