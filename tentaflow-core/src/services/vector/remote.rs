// ============ File: services/vector/remote.rs — cross-node Milvus over the mesh (VectorOp proxy) ============
//
// A `RemoteMeshVectorBackend` lets an addon on THIS node store/query vectors in
// a Milvus service owned by ANOTHER mesh node. Every `VectorBackend` op is
// encoded as a minicbor `VectorOpRequest` and shipped via `RemoteVectorTransport`
// to the owner, which opens its OWN local Milvus (by `service_id`) and runs the
// op, returning a `VectorOpResponse`. This mirrors the WebResearch mesh-proxy:
// the owner executes against its loopback service, so no Milvus port is ever
// exposed across nodes — and THIS node needs no `vector-milvus` feature to be a
// client (only the owner connects to Milvus). The collection name is precomputed
// here (deterministic isolation), so the owner never consults addon config and
// the proxy is recursion-free by construction.

use std::sync::Arc;

use super::backend::{
    Field, FieldSpec, Filter, Fusion, Metric, SearchHit, SparseVector, VectorBackend,
};
use super::error::{Result, VectorError};

/// Blocking transport that ships an opaque VectorOp request CBOR to a mesh node
/// and returns the VectorOp response CBOR. The concrete impl (mesh layer) wraps
/// `IrohMeshManager::send_command` plus the async→sync bridge and the protocol
/// `MeshCommandType::VectorOp` marshaling, keeping this module free of any
/// iroh/protocol knowledge (and trivially mockable in tests).
pub trait RemoteVectorTransport: Send + Sync {
    fn execute(&self, node_id: &str, request_cbor: Vec<u8>) -> Result<Vec<u8>>;
}

// Wire-stable metric tags — do NOT renumber (they travel in VectorOpRequest).
fn metric_tag(m: Metric) -> u8 {
    match m {
        Metric::Cosine => 0,
        Metric::Euclidean => 1,
        Metric::Dot => 2,
    }
}

/// Inverse of [`metric_tag`], for the owner side decoding a request.
pub fn metric_from_tag(t: u8) -> Option<Metric> {
    match t {
        0 => Some(Metric::Cosine),
        1 => Some(Metric::Euclidean),
        2 => Some(Metric::Dot),
        _ => None,
    }
}

/// Mirror of [`SearchHit`] with minicbor derives (SearchHit itself has none).
#[derive(minicbor::Encode, minicbor::Decode)]
pub struct RemoteHit {
    #[n(0)]
    pub ref_id: u64,
    #[n(1)]
    pub score: f32,
    #[n(2)]
    pub fields: Vec<Field>,
}

/// One forwarded vector operation + its arguments (mirrors the `VectorBackend`
/// trait surface). `dim`/`metric` are NOT here — they live in `VectorOpRequest`
/// because the owner needs them to open the collection regardless of the op.
#[derive(minicbor::Encode, minicbor::Decode)]
pub enum VectorOp {
    #[n(0)]
    Upsert {
        #[n(0)]
        ref_id: u64,
        #[n(1)]
        vector: Vec<f32>,
        #[n(2)]
        fields: Vec<Field>,
        #[n(3)]
        sparse: Option<SparseVector>,
    },
    #[n(1)]
    Search {
        #[n(0)]
        query: Vec<f32>,
        #[n(1)]
        k: u64,
        #[n(2)]
        filter: Option<Filter>,
        #[n(3)]
        output_fields: Vec<String>,
    },
    #[n(2)]
    HybridSearch {
        #[n(0)]
        dense: Vec<f32>,
        #[n(1)]
        sparse: SparseVector,
        #[n(2)]
        k: u64,
        #[n(3)]
        filter: Option<Filter>,
        #[n(4)]
        output_fields: Vec<String>,
        #[n(5)]
        fusion: Fusion,
    },
    #[n(3)]
    Delete {
        #[n(0)]
        ref_id: u64,
    },
    #[n(4)]
    HasRef {
        #[n(0)]
        ref_id: u64,
    },
    #[n(5)]
    Count,
    #[n(6)]
    Save,
    #[n(7)]
    ReconcileFields {
        #[n(0)]
        stored: Vec<FieldSpec>,
        #[n(1)]
        desired: Vec<FieldSpec>,
    },
}

/// Everything the owner needs to open ITS local Milvus and run one op. The
/// collection name is precomputed by the client (deterministic isolation), so
/// the owner never reads addon config — this is what makes the proxy
/// recursion-free (the owner always resolves to a concrete local Milvus).
#[derive(minicbor::Encode, minicbor::Decode)]
pub struct VectorOpRequest {
    /// Milvus service id ON THE OWNER node (from the addon's service_ref).
    #[n(0)]
    pub service_id: String,
    #[n(1)]
    pub collection: String,
    #[n(2)]
    pub dim: u32,
    #[n(3)]
    pub metric_tag: u8,
    #[n(4)]
    pub fields: Vec<FieldSpec>,
    #[n(5)]
    pub sparse: bool,
    /// Credentials for the owner's Milvus, supplied by the client's addon config
    /// (the addon configured "use node B's Milvus with these creds"). Sent over
    /// the encrypted mesh channel within the node trust model.
    #[n(6)]
    pub user: Option<String>,
    #[n(7)]
    pub password: Option<String>,
    #[n(8)]
    pub op: VectorOp,
}

/// Result of a forwarded op. `Err` carries the owner's failure message.
#[derive(minicbor::Encode, minicbor::Decode)]
pub enum VectorOpResponse {
    #[n(0)]
    Unit,
    #[n(1)]
    Bool(#[n(0)] bool),
    #[n(2)]
    Count(#[n(0)] u64),
    #[n(3)]
    Hits(#[n(0)] Vec<RemoteHit>),
    #[n(4)]
    Err(#[n(0)] String),
}

/// `VectorBackend` whose ops execute on a remote node's Milvus over the mesh.
/// Construction (and the descriptor it carries) happens in `NamespaceManager`
/// when an addon's `__vector_config` points at a `service_ref` with a non-empty
/// `node_id`.
pub struct RemoteMeshVectorBackend {
    transport: Arc<dyn RemoteVectorTransport>,
    node_id: String,
    service_id: String,
    collection: String,
    dim: u32,
    metric: Metric,
    fields: Vec<FieldSpec>,
    sparse: bool,
    user: Option<String>,
    password: Option<String>,
}

impl RemoteMeshVectorBackend {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        transport: Arc<dyn RemoteVectorTransport>,
        node_id: String,
        service_id: String,
        collection: String,
        dim: u32,
        metric: Metric,
        fields: Vec<FieldSpec>,
        sparse: bool,
        user: Option<String>,
        password: Option<String>,
    ) -> Self {
        Self {
            transport,
            node_id,
            service_id,
            collection,
            dim,
            metric,
            fields,
            sparse,
            user,
            password,
        }
    }

    /// Encode the op into a request, ship it, decode the response. Maps a remote
    /// `Err` payload (and any transport failure) to `VectorError::Backend`.
    fn call(&self, op: VectorOp) -> Result<VectorOpResponse> {
        let req = VectorOpRequest {
            service_id: self.service_id.clone(),
            collection: self.collection.clone(),
            dim: self.dim,
            metric_tag: metric_tag(self.metric),
            fields: self.fields.clone(),
            sparse: self.sparse,
            user: self.user.clone(),
            password: self.password.clone(),
            op,
        };
        let bytes = minicbor::to_vec(&req)
            .map_err(|e| VectorError::Backend(format!("encode VectorOpRequest: {e}")))?;
        let resp_bytes = self.transport.execute(&self.node_id, bytes)?;
        let resp: VectorOpResponse = minicbor::decode(&resp_bytes)
            .map_err(|e| VectorError::Backend(format!("decode VectorOpResponse: {e}")))?;
        if let VectorOpResponse::Err(msg) = &resp {
            return Err(VectorError::Backend(format!(
                "zdalny Milvus (node {}): {msg}",
                self.node_id
            )));
        }
        Ok(resp)
    }
}

const UNEXPECTED: &str = "zdalny Milvus: nieoczekiwany typ odpowiedzi";

impl VectorBackend for RemoteMeshVectorBackend {
    fn upsert(
        &self,
        ref_id: u64,
        vector: &[f32],
        fields: &[Field],
        sparse: Option<&SparseVector>,
    ) -> Result<()> {
        self.call(VectorOp::Upsert {
            ref_id,
            vector: vector.to_vec(),
            fields: fields.to_vec(),
            sparse: sparse.cloned(),
        })?;
        Ok(())
    }

    fn search(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&Filter>,
        output_fields: &[String],
    ) -> Result<Vec<SearchHit>> {
        match self.call(VectorOp::Search {
            query: query.to_vec(),
            k: k as u64,
            filter: filter.cloned(),
            output_fields: output_fields.to_vec(),
        })? {
            VectorOpResponse::Hits(hits) => Ok(hits
                .into_iter()
                .map(|h| SearchHit {
                    ref_id: h.ref_id,
                    score: h.score,
                    fields: h.fields,
                })
                .collect()),
            _ => Err(VectorError::Backend(UNEXPECTED.to_string())),
        }
    }

    fn hybrid_search(
        &self,
        dense: &[f32],
        sparse: &SparseVector,
        k: usize,
        filter: Option<&Filter>,
        output_fields: &[String],
        fusion: Fusion,
    ) -> Result<Vec<SearchHit>> {
        match self.call(VectorOp::HybridSearch {
            dense: dense.to_vec(),
            sparse: sparse.clone(),
            k: k as u64,
            filter: filter.cloned(),
            output_fields: output_fields.to_vec(),
            fusion,
        })? {
            VectorOpResponse::Hits(hits) => Ok(hits
                .into_iter()
                .map(|h| SearchHit {
                    ref_id: h.ref_id,
                    score: h.score,
                    fields: h.fields,
                })
                .collect()),
            _ => Err(VectorError::Backend(UNEXPECTED.to_string())),
        }
    }

    fn delete(&self, ref_id: u64) -> Result<bool> {
        match self.call(VectorOp::Delete { ref_id })? {
            VectorOpResponse::Bool(b) => Ok(b),
            _ => Err(VectorError::Backend(UNEXPECTED.to_string())),
        }
    }

    // Infallible trait method — degrade to `false` on any transport/remote error
    // (panicking a backend because the mesh blipped would be worse). Logged.
    fn has_ref(&self, ref_id: u64) -> bool {
        match self.call(VectorOp::HasRef { ref_id }) {
            Ok(VectorOpResponse::Bool(b)) => b,
            Ok(_) => false,
            Err(e) => {
                tracing::warn!(node = %self.node_id, error = %e, "remote has_ref failed");
                false
            }
        }
    }

    // Infallible — degrade to 0 on error (logged). Callers use this for quota
    // accounting; a transient 0 is safe (no silent data loss, just a stale count).
    fn count(&self) -> u64 {
        match self.call(VectorOp::Count) {
            Ok(VectorOpResponse::Count(c)) => c,
            Ok(_) => 0,
            Err(e) => {
                tracing::warn!(node = %self.node_id, error = %e, "remote count failed");
                0
            }
        }
    }

    fn save(&self) -> Result<()> {
        self.call(VectorOp::Save)?;
        Ok(())
    }

    fn reconcile_fields(&self, stored: &[FieldSpec], desired: &[FieldSpec]) -> Result<()> {
        self.call(VectorOp::ReconcileFields {
            stored: stored.to_vec(),
            desired: desired.to_vec(),
        })?;
        Ok(())
    }

    fn dim(&self) -> u32 {
        self.dim
    }

    fn metric(&self) -> Metric {
        self.metric
    }
}

// ============================================================================
// Owner side: run a forwarded op against THIS node's local Milvus.
// ============================================================================

/// Decode a forwarded request, run it against the LOCAL Milvus, encode the
/// response. NEVER returns Err — every failure is encoded as
/// `VectorOpResponse::Err` so the consumer maps it to a backend error. Called by
/// the mesh command executor on a blocking task.
pub fn handle_vector_op_cbor(db: &crate::db::DbPool, request_cbor: &[u8]) -> Vec<u8> {
    let resp = match minicbor::decode::<VectorOpRequest>(request_cbor) {
        Ok(req) => execute_owner(db, req),
        Err(e) => VectorOpResponse::Err(format!("decode VectorOpRequest: {e}")),
    };
    minicbor::to_vec(&resp).unwrap_or_else(|_| {
        // A String-only Err is the smallest encodable response; fall back to it
        // so the consumer still decodes a real error rather than empty bytes.
        minicbor::to_vec(&VectorOpResponse::Err(
            "owner: encode VectorOpResponse failed".to_string(),
        ))
        .unwrap_or_default()
    })
}

#[cfg(not(feature = "vector-milvus"))]
fn execute_owner(_db: &crate::db::DbPool, _req: VectorOpRequest) -> VectorOpResponse {
    VectorOpResponse::Err(
        "ten node nie ma wkompilowanego Milvus (feature vector-milvus) — nie moze byc \
         wlascicielem zdalnego vector store"
            .to_string(),
    )
}

#[cfg(feature = "vector-milvus")]
fn execute_owner(db: &crate::db::DbPool, req: VectorOpRequest) -> VectorOpResponse {
    owner::execute(db, req)
}

/// Local Milvus execution + a process-wide connection cache (keyed by the full
/// backend identity) so we do not reconnect gRPC per op. Recursion-free: resolves
/// ONLY the node's own loopback service by id — never a remote ref.
///
/// Trust model: the mesh command executor rejects untrusted peers before
/// dispatch, so only trust-paired (same-operator-fleet) nodes reach here — the
/// same boundary as the WebResearch / frame-pickup proxies. `valid_collection`
/// additionally confines ops to the deterministic isolation collection form so a
/// trusted-but-buggy peer cannot steer at arbitrary non-TentaFlow collections.
/// Cross-tenant authorization across nodes is the consumer's org-isolation
/// concern and is not re-enforced here.
#[cfg(feature = "vector-milvus")]
mod owner {
    use super::{metric_from_tag, RemoteHit, VectorOp, VectorOpRequest, VectorOpResponse};
    use crate::db::DbPool;
    use crate::services::vector::backend::{SearchHit, VectorBackend};
    use crate::services::vector::error::{Result, VectorError};
    use dashmap::DashMap;
    use std::sync::{Arc, OnceLock};

    // Defense-in-depth bounds on a forwarded request (the peer is trust-paired,
    // but we still refuse pathological inputs before touching Milvus).
    const MAX_DIM: u32 = 4096;
    const MAX_K: u64 = 4096;
    const MAX_FIELDS: usize = 64;
    const MAX_OUTPUT_FIELDS: usize = 64;
    const MAX_SPARSE_TERMS: usize = 65_536;

    /// Sparse vector bound: equal-length, non-empty, capped terms.
    fn check_sparse(s: &super::SparseVector) -> Result<()> {
        if s.indices.len() != s.values.len() {
            return Err(VectorError::Backend(
                "sparse: indices/values rozna dlugosc".to_string(),
            ));
        }
        if s.indices.len() > MAX_SPARSE_TERMS {
            return Err(VectorError::Backend("sparse: za duzo termow".to_string()));
        }
        Ok(())
    }

    /// Connection cache keyed by the FULL backend identity (not just
    /// service_id|collection) so a request with different endpoint / auth /
    /// schema can never reuse — and thus poison — another's connection.
    fn cache() -> &'static DashMap<String, Arc<dyn VectorBackend>> {
        static C: OnceLock<DashMap<String, Arc<dyn VectorBackend>>> = OnceLock::new();
        C.get_or_init(DashMap::new)
    }

    /// Resolve the LOCAL Milvus service by id → loopback endpoint. Same filter as
    /// the local picker: engine milvus, running/degraded, not paused, endpoint set.
    fn local_service_endpoint(db: &DbPool, service_id: &str) -> Option<String> {
        use crate::services_repo::services::ServiceStatus;
        let id: i64 = service_id.parse().ok()?;
        let conn = db.read().ok()?;
        let services = crate::services_repo::services::list_all(&conn).ok()?;
        services
            .into_iter()
            .find(|s| {
                s.id == id
                    && s.engine_id == "milvus"
                    && !s.paused
                    && matches!(s.status, ServiceStatus::Running | ServiceStatus::Degraded)
            })
            .and_then(|s| s.endpoint_url)
            .filter(|u| !u.is_empty())
    }

    /// The collection name MUST be the deterministic isolation form produced by
    /// `milvus_collection_name` (`v_o_<12hex>_a_<12hex>_n_<12hex>[_suffix]`).
    /// Refusing other shapes stops a peer from steering ops at arbitrary
    /// pre-existing collections on this node's Milvus.
    fn valid_collection(name: &str) -> bool {
        let hex12 = |s: &str| s.len() == 12 && s.bytes().all(|b| b.is_ascii_hexdigit());
        let Some(rest) = name.strip_prefix("v_o_") else {
            return false;
        };
        let Some((org, rest)) = rest.split_once("_a_") else {
            return false;
        };
        let Some((addon, rest)) = rest.split_once("_n_") else {
            return false;
        };
        let (ns, suffix) = match rest.split_once('_') {
            Some((ns, suf)) => (ns, Some(suf)),
            None => (rest, None),
        };
        if !(hex12(org) && hex12(addon) && hex12(ns)) {
            return false;
        }
        match suffix {
            None => true,
            Some(suf) => {
                !suf.is_empty()
                    && suf.len() <= 32
                    && suf.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            }
        }
    }

    /// Reject pathological / malformed requests before opening Milvus.
    fn validate(req: &VectorOpRequest) -> Result<()> {
        if !(1..=MAX_DIM).contains(&req.dim) {
            return Err(VectorError::Backend(format!(
                "dim {} poza zakresem",
                req.dim
            )));
        }
        if !valid_collection(&req.collection) {
            return Err(VectorError::Backend(
                "nazwa kolekcji nie ma dozwolonej formy izolacji".to_string(),
            ));
        }
        if req.fields.len() > MAX_FIELDS {
            return Err(VectorError::Backend("za duzo pol schematu".to_string()));
        }
        let dim = req.dim as usize;
        match &req.op {
            VectorOp::Upsert {
                vector,
                fields,
                sparse,
                ..
            } => {
                if vector.len() != dim {
                    return Err(VectorError::Backend("dlugosc wektora != dim".to_string()));
                }
                if fields.len() > MAX_FIELDS {
                    return Err(VectorError::Backend("za duzo pol w upsert".to_string()));
                }
                if let Some(s) = sparse {
                    check_sparse(s)?;
                }
            }
            VectorOp::Search {
                query,
                k,
                output_fields,
                ..
            } => {
                if query.len() != dim {
                    return Err(VectorError::Backend("dlugosc query != dim".to_string()));
                }
                if *k == 0 || *k > MAX_K {
                    return Err(VectorError::Backend(format!("k {k} poza zakresem")));
                }
                if output_fields.len() > MAX_OUTPUT_FIELDS {
                    return Err(VectorError::Backend("za duzo output_fields".to_string()));
                }
            }
            VectorOp::HybridSearch {
                dense,
                sparse,
                k,
                output_fields,
                ..
            } => {
                if dense.len() != dim {
                    return Err(VectorError::Backend("dlugosc dense != dim".to_string()));
                }
                if *k == 0 || *k > MAX_K {
                    return Err(VectorError::Backend(format!("k {k} poza zakresem")));
                }
                if output_fields.len() > MAX_OUTPUT_FIELDS {
                    return Err(VectorError::Backend("za duzo output_fields".to_string()));
                }
                check_sparse(sparse)?;
            }
            VectorOp::ReconcileFields { stored, desired } => {
                if stored.len() > MAX_FIELDS || desired.len() > MAX_FIELDS {
                    return Err(VectorError::Backend("za duzo pol w reconcile".to_string()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Stable cache key over the full connection + schema + auth identity.
    fn cache_key(endpoint: &str, req: &VectorOpRequest) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        for part in [
            endpoint,
            &req.service_id,
            &req.collection,
            &req.dim.to_string(),
            &req.metric_tag.to_string(),
            &(req.sparse as u8).to_string(),
            req.user.as_deref().unwrap_or(""),
            req.password.as_deref().unwrap_or(""),
        ] {
            h.update(part.as_bytes());
            h.update([0u8]); // field separator — no boundary ambiguity
        }
        for f in &req.fields {
            h.update(f.name.as_bytes());
            h.update([0u8]);
        }
        format!("{:x}", h.finalize())
    }

    fn open(db: &DbPool, req: &VectorOpRequest) -> Result<(String, Arc<dyn VectorBackend>)> {
        let endpoint = local_service_endpoint(db, &req.service_id).ok_or_else(|| {
            VectorError::Backend(format!(
                "lokalny serwis Milvus '{}' nieosiagalny (running + endpoint)",
                req.service_id
            ))
        })?;
        let key = cache_key(&endpoint, req);
        if let Some(be) = cache().get(&key) {
            return Ok((key, be.clone()));
        }
        let metric = metric_from_tag(req.metric_tag).ok_or_else(|| {
            VectorError::Backend(format!("nieznany metric_tag {}", req.metric_tag))
        })?;
        let be = crate::services::vector::milvus_backend::MilvusBackend::connect(
            &endpoint,
            req.user.as_deref(),
            req.password.as_deref(),
            &req.collection,
            req.dim,
            metric,
            &req.fields,
            req.sparse,
        )?;
        let arc: Arc<dyn VectorBackend> = Arc::new(be);
        cache().insert(key.clone(), arc.clone());
        Ok((key, arc))
    }

    pub fn execute(db: &DbPool, req: VectorOpRequest) -> VectorOpResponse {
        if let Err(e) = validate(&req) {
            return VectorOpResponse::Err(e.to_string());
        }
        let (key, be) = match open(db, &req) {
            Ok(b) => b,
            Err(e) => return VectorOpResponse::Err(e.to_string()),
        };
        match run(be.as_ref(), req.op) {
            Ok(resp) => resp,
            Err(e) => {
                // Drop a possibly-broken connection so the next op reconnects
                // (e.g. after a Milvus restart on the same endpoint).
                cache().remove(&key);
                VectorOpResponse::Err(e.to_string())
            }
        }
    }

    fn run(be: &dyn VectorBackend, op: VectorOp) -> Result<VectorOpResponse> {
        Ok(match op {
            VectorOp::Upsert {
                ref_id,
                vector,
                fields,
                sparse,
            } => {
                be.upsert(ref_id, &vector, &fields, sparse.as_ref())?;
                VectorOpResponse::Unit
            }
            VectorOp::Search {
                query,
                k,
                filter,
                output_fields,
            } => {
                let hits = be.search(&query, k as usize, filter.as_ref(), &output_fields)?;
                VectorOpResponse::Hits(into_remote_hits(hits))
            }
            VectorOp::HybridSearch {
                dense,
                sparse,
                k,
                filter,
                output_fields,
                fusion,
            } => {
                let hits = be.hybrid_search(
                    &dense,
                    &sparse,
                    k as usize,
                    filter.as_ref(),
                    &output_fields,
                    fusion,
                )?;
                VectorOpResponse::Hits(into_remote_hits(hits))
            }
            VectorOp::Delete { ref_id } => VectorOpResponse::Bool(be.delete(ref_id)?),
            VectorOp::HasRef { ref_id } => VectorOpResponse::Bool(be.has_ref(ref_id)),
            VectorOp::Count => VectorOpResponse::Count(be.count()),
            VectorOp::Save => {
                be.save()?;
                VectorOpResponse::Unit
            }
            VectorOp::ReconcileFields { stored, desired } => {
                be.reconcile_fields(&stored, &desired)?;
                VectorOpResponse::Unit
            }
        })
    }

    fn into_remote_hits(hits: Vec<SearchHit>) -> Vec<RemoteHit> {
        hits.into_iter()
            .map(|h| RemoteHit {
                ref_id: h.ref_id,
                score: h.score,
                fields: h.fields,
            })
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::valid_collection;

        #[test]
        fn collection_format_guard() {
            // Canonical isolation forms produced by milvus_collection_name.
            assert!(valid_collection(
                "v_o_0123456789ab_a_0123456789ab_n_0123456789ab"
            ));
            assert!(valid_collection(
                "v_o_0123456789ab_a_0123456789ab_n_0123456789ab_prod"
            ));
            // Rejections: arbitrary names, non-hex segments, missing parts,
            // empty / illegal suffix.
            assert!(!valid_collection("some_other_collection"));
            assert!(!valid_collection("v_o_xyz_a_0123456789ab_n_0123456789ab"));
            assert!(!valid_collection("v_o_0123456789ab_a_0123456789ab"));
            assert!(!valid_collection(
                "v_o_0123456789ab_a_0123456789ab_n_0123456789ab_"
            ));
            assert!(!valid_collection(
                "v_o_0123456789ab_a_0123456789ab_n_0123456789ab_bad-suffix"
            ));
        }
    }
}
