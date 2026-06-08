// ============ File: services/vector/milvus_backend.rs — external Milvus backend ============
//
// `VectorBackend` over an external Milvus server via the official gRPC client.
// Selectable per addon (admin deploys Milvus + picks it in addon settings); the
// embedded zvec backend remains the default. One namespace maps to one Milvus
// collection with a fixed schema: an int64 primary key (`ref_id`) + a float
// vector field (`vec`) indexed with HNSW.
//
// The Milvus client is async (tokio) but `VectorBackend` is synchronous, so this
// backend owns a dedicated multi-thread runtime and bridges each call by spawning
// the future onto it and blocking the caller on a std channel. We deliberately do
// NOT use `Runtime::block_on`, which panics when invoked from within the server's
// own runtime; spawning + a blocking `recv` is safe from any thread.

use std::collections::HashMap;
use std::future::Future;
use std::sync::mpsc::sync_channel;

use std::borrow::Cow;

use milvus::client::{Client, ClientBuilder};
use milvus::data::FieldColumn;
use milvus::index::{IndexParams, IndexType, MetricType};
use milvus::mutate::{DeleteOptions, UpsertOptions};
use milvus::proto::common::KeyValuePair;
use milvus::proto::schema::SparseFloatArray;
use milvus::query::{
    AnnSearchRequest, BaseRanker, QueryOptions, RrfRanker, SearchOptions, WeightedRanker,
};
use milvus::schema::{CollectionSchemaBuilder, FieldSchema};
use milvus::value::{Value, ValueVec};
use tokio::runtime::{Builder, Runtime};

use super::backend::{
    Field, FieldSpec, FieldValue, Filter, Fusion, Metric, SearchHit, SparseVector, VectorBackend,
};
use super::error::{Result, VectorError};
use super::filter;
use tentaflow_sdk_spec::FieldType;

const PK_FIELD: &str = "ref_id";
const VEC_FIELD: &str = "vec";
const SPARSE_FIELD: &str = "sparse";

/// Serialize one sparse vector into Milvus's row byte format: pairs of
/// `(u32 index LE, f32 value LE)` sorted by ascending index. Returns the row
/// bytes plus the implied dimensionality (max index + 1).
fn sparse_row(indices: &[u32], values: &[f32]) -> (Vec<u8>, u32) {
    let mut pairs: Vec<(u32, f32)> = indices
        .iter()
        .copied()
        .zip(values.iter().copied())
        .collect();
    pairs.sort_by_key(|(i, _)| *i);
    let mut bytes = Vec::with_capacity(pairs.len() * 8);
    let mut dim = 0u32;
    for (i, v) in pairs {
        bytes.extend_from_slice(&i.to_le_bytes());
        bytes.extend_from_slice(&v.to_le_bytes());
        dim = dim.max(i + 1);
    }
    (bytes, dim)
}

/// A one-row `SparseFloatArray` for the given sparse vector.
fn sparse_array(sparse: &SparseVector) -> SparseFloatArray {
    let (row, dim) = sparse_row(&sparse.indices, &sparse.values);
    SparseFloatArray {
        contents: vec![row],
        dim: dim as i64,
    }
}

/// Build a Milvus scalar field schema for a declared metadata field.
fn scalar_field(spec: &FieldSpec) -> FieldSchema {
    match spec.field_type {
        FieldType::Str => FieldSchema::new_varchar(&spec.name, "", 65535),
        FieldType::Int => FieldSchema::new_int64(&spec.name, ""),
        FieldType::Float => FieldSchema::new_double(&spec.name, ""),
        FieldType::Bool => FieldSchema::new_bool(&spec.name, ""),
    }
}

/// One FieldColumn for a single metadata value (Milvus inserts are columnar; we
/// insert one row at a time).
fn value_column(field: &Field) -> FieldColumn {
    match &field.value {
        FieldValue::Str(s) => FieldColumn::new(
            &FieldSchema::new_varchar(&field.name, "", 65535),
            vec![s.clone()],
        ),
        FieldValue::Int(i) => FieldColumn::new(&FieldSchema::new_int64(&field.name, ""), vec![*i]),
        FieldValue::Float(f) => {
            FieldColumn::new(&FieldSchema::new_double(&field.name, ""), vec![*f])
        }
        FieldValue::Bool(b) => FieldColumn::new(&FieldSchema::new_bool(&field.name, ""), vec![*b]),
    }
}

type MilvusResult<T> = std::result::Result<T, milvus::error::Error>;

/// Map a Milvus scalar cell to our universal `FieldValue`. Returns `None` for
/// NULL or for types that have no universal counterpart (vectors, arrays, …) —
/// those are simply omitted from the returned field set.
fn value_to_field_value(v: &Value) -> Option<FieldValue> {
    match v {
        Value::Bool(b) => Some(FieldValue::Bool(*b)),
        Value::Int8(i) => Some(FieldValue::Int(*i as i64)),
        Value::Int16(i) => Some(FieldValue::Int(*i as i64)),
        Value::Int32(i) => Some(FieldValue::Int(*i as i64)),
        Value::Long(i) => Some(FieldValue::Int(*i)),
        Value::Float(f) => Some(FieldValue::Float(*f as f64)),
        Value::Double(f) => Some(FieldValue::Float(*f)),
        Value::String(s) => Some(FieldValue::Str(s.to_string())),
        _ => None,
    }
}

fn metric_type(m: Metric) -> MetricType {
    match m {
        Metric::Cosine => MetricType::COSINE,
        Metric::Euclidean => MetricType::L2,
        Metric::Dot => MetricType::IP,
    }
}

fn metric_param(m: Metric) -> &'static str {
    match m {
        Metric::Cosine => "COSINE",
        Metric::Euclidean => "L2",
        Metric::Dot => "IP",
    }
}

fn backend_err<E: std::fmt::Display>(e: E) -> VectorError {
    VectorError::Backend(e.to_string())
}

/// Spawn `fut` on `rt` and block the current thread until it resolves. Safe to
/// call from inside another tokio runtime (unlike `Runtime::block_on`).
fn run_blocking<T, F>(rt: &Runtime, fut: F) -> T
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    let (tx, rx) = sync_channel(1);
    rt.spawn(async move {
        let _ = tx.send(fut.await);
    });
    rx.recv()
        .expect("milvus runtime task dropped before completion")
}

pub struct MilvusBackend {
    rt: Runtime,
    client: Client,
    collection: String,
    dim: u32,
    metric: Metric,
    sparse: bool,
}

impl MilvusBackend {
    /// Connect to Milvus at `url` (optionally authenticated) and ensure the
    /// collection for this namespace exists (creating it + an HNSW index on first
    /// use), then load it.
    pub fn connect(
        url: &str,
        user: Option<&str>,
        password: Option<&str>,
        collection: &str,
        dim: u32,
        metric: Metric,
        fields: &[FieldSpec],
        sparse: bool,
    ) -> Result<Self> {
        if !(1..=4096).contains(&dim) {
            return Err(VectorError::InvalidDim(dim));
        }
        let rt = Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| VectorError::Backend(format!("tokio runtime: {e}")))?;

        let url = url.to_string();
        let user = user.map(|s| s.to_string());
        let password = password.map(|s| s.to_string());
        let coll = collection.to_string();
        let coll_async = coll.clone();
        let fields = fields.to_vec();
        let client = run_blocking(&rt, async move {
            let client = match (user, password) {
                (Some(u), Some(p)) => {
                    ClientBuilder::new(url)
                        .username(&u)
                        .password(&p)
                        .build()
                        .await?
                }
                _ => Client::new(url).await?,
            };
            if !client.has_collection(coll_async.clone()).await? {
                let mut builder =
                    CollectionSchemaBuilder::new(&coll_async, "tentaflow vector namespace");
                builder.add_field(FieldSchema::new_primary_int64(PK_FIELD, "ref id", false));
                builder.add_field(FieldSchema::new_float_vector(
                    VEC_FIELD,
                    "embedding",
                    dim as i64,
                ));
                for spec in &fields {
                    builder.add_field(scalar_field(spec));
                }
                if sparse {
                    builder.add_field(FieldSchema::new_sparse_float_vector(SPARSE_FIELD, ""));
                }
                let schema = builder.build()?;
                client.create_collection(schema, None).await?;
                let index = IndexParams::new(
                    "vec_idx".to_string(),
                    IndexType::HNSW,
                    metric_type(metric),
                    HashMap::from([
                        ("M".to_string(), "16".to_string()),
                        ("efConstruction".to_string(), "200".to_string()),
                    ]),
                );
                client
                    .create_index(coll_async.clone(), VEC_FIELD, index)
                    .await?;
                if sparse {
                    // Sparse vectors use an inverted index with inner-product metric.
                    let sp_index = IndexParams::new(
                        "sparse_idx".to_string(),
                        IndexType::SparseInvertedIndex,
                        MetricType::IP,
                        HashMap::new(),
                    );
                    client
                        .create_index(coll_async.clone(), SPARSE_FIELD, sp_index)
                        .await?;
                }
            }
            client.load_collection(coll_async, None).await?;
            Ok::<Client, milvus::error::Error>(client)
        })
        .map_err(backend_err)?;

        Ok(Self {
            rt,
            client,
            collection: coll,
            dim,
            metric,
            sparse,
        })
    }

    fn block<T, F>(&self, fut: F) -> T
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        run_blocking(&self.rt, fut)
    }
}

/// Map one Milvus `SearchResult` (columnar) into our `SearchHit`s, pulling each
/// requested `output_field` at the row index. Shared by dense and hybrid search.
fn result_to_hits(
    result: Option<milvus::collection::SearchResult<'_>>,
    output_fields: &[String],
) -> Vec<SearchHit> {
    let mut out = Vec::new();
    if let Some(r) = result {
        for (row, (idv, score)) in r.id.iter().zip(r.score.iter()).enumerate() {
            let Ok(id) = i64::try_from(idv.clone()) else {
                continue;
            };
            let mut fields = Vec::new();
            for name in output_fields {
                if let Some(col) = r.field.iter().find(|c| &c.name == name) {
                    if let Some(value) = col.get(row).and_then(|v| value_to_field_value(&v)) {
                        fields.push(Field {
                            name: name.clone(),
                            value,
                        });
                    }
                }
            }
            out.push(SearchHit {
                ref_id: id as u64,
                score: *score,
                fields,
            });
        }
    }
    out
}

impl VectorBackend for MilvusBackend {
    fn upsert(
        &self,
        ref_id: u64,
        vector: &[f32],
        fields: &[Field],
        sparse: Option<&SparseVector>,
    ) -> Result<()> {
        if ref_id == 0 {
            return Err(VectorError::InvalidRefId);
        }
        if vector.is_empty() {
            return Err(VectorError::EmptyVector);
        }
        if vector.len() as u32 != self.dim {
            return Err(VectorError::DimMismatch {
                expected: self.dim,
                actual: vector.len() as u32,
            });
        }
        if sparse.is_some() && !self.sparse {
            return Err(VectorError::Backend(
                "namespace does not support sparse vectors (declare sparse = true)".into(),
            ));
        }
        let client = self.client.clone();
        let coll = self.collection.clone();
        let dim = self.dim as i64;
        let values = vector.to_vec();
        let id = ref_id as i64;
        let fields = fields.to_vec();
        let sparse_arr = sparse.map(sparse_array);
        self.block(async move {
            let pk_field = FieldSchema::new_primary_int64(PK_FIELD, "ref id", false);
            let vec_field = FieldSchema::new_float_vector(VEC_FIELD, "embedding", dim);
            let mut cols = vec![
                FieldColumn::new(&pk_field, vec![id]),
                FieldColumn::new(&vec_field, values),
            ];
            for f in &fields {
                cols.push(value_column(f));
            }
            if let Some(arr) = sparse_arr {
                cols.push(FieldColumn {
                    name: SPARSE_FIELD.to_string(),
                    dtype: milvus::proto::schema::DataType::SparseFloatVector,
                    value: ValueVec::SparseFloat(arr),
                    dim: 0,
                    max_length: 0,
                    is_dynamic: false,
                });
            }
            // No explicit flush: Milvus persists via its own WAL (durable once the
            // RPC returns), and flushing per-write seals tiny segments that then
            // need re-indexing before they are searchable — hurting latency.
            client.upsert(coll, cols, None::<UpsertOptions>).await?;
            Ok::<(), milvus::error::Error>(())
        })
        .map_err(backend_err)
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
        if !self.sparse {
            return Err(VectorError::Backend(
                "namespace does not support hybrid search (declare sparse = true)".into(),
            ));
        }
        if dense.is_empty() {
            return Err(VectorError::EmptyVector);
        }
        if dense.len() as u32 != self.dim {
            return Err(VectorError::DimMismatch {
                expected: self.dim,
                actual: dense.len() as u32,
            });
        }
        let filter_expr = match filter {
            Some(f) => Some(filter::to_milvus(f)?),
            None => None,
        };
        let client = self.client.clone();
        let coll = self.collection.clone();
        let dense_q = dense.to_vec();
        let sparse_arr = sparse_array(sparse);
        let metric = metric_param(self.metric);
        let out_fields = output_fields.to_vec();
        let hits: MilvusResult<Vec<SearchHit>> = self.block(async move {
            let dense_param = vec![KeyValuePair {
                key: "metric_type".to_string(),
                value: metric.to_string(),
            }];
            let sparse_param = vec![KeyValuePair {
                key: "metric_type".to_string(),
                value: "IP".to_string(),
            }];
            let mut dense_req = AnnSearchRequest::new(
                vec![Value::from(dense_q)],
                VEC_FIELD.to_string(),
                dense_param,
                k,
            );
            let mut sparse_req = AnnSearchRequest::new(
                vec![Value::SparseFloat(Cow::Owned(sparse_arr))],
                SPARSE_FIELD.to_string(),
                sparse_param,
                k,
            );
            if let Some(expr) = &filter_expr {
                dense_req.expr = Some(expr.clone());
                sparse_req.expr = Some(expr.clone());
            }
            let ranker: Box<dyn BaseRanker> = match fusion {
                Fusion::Rrf(c) => Box::new(RrfRanker::new(c as f64)),
                Fusion::Weighted(d, s) => Box::new(WeightedRanker::new(vec![d as f64, s as f64])),
            };
            let results = client
                .hybrid_search(coll, vec![dense_req, sparse_req], ranker, None)
                .await?;
            Ok(result_to_hits(results.into_iter().next(), &out_fields))
        });
        hits.map_err(backend_err)
    }

    fn search(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&Filter>,
        output_fields: &[String],
    ) -> Result<Vec<SearchHit>> {
        if query.is_empty() {
            return Err(VectorError::EmptyVector);
        }
        if query.len() as u32 != self.dim {
            return Err(VectorError::DimMismatch {
                expected: self.dim,
                actual: query.len() as u32,
            });
        }
        let filter_expr = match filter {
            Some(f) => Some(filter::to_milvus(f)?),
            None => None,
        };
        let client = self.client.clone();
        let coll = self.collection.clone();
        let q = query.to_vec();
        let metric = metric_param(self.metric);
        let out_fields = output_fields.to_vec();
        let hits: MilvusResult<Vec<SearchHit>> = self.block(async move {
            let mut options = SearchOptions::new()
                .limit(k)
                .add_param("anns_field", VEC_FIELD)
                .add_param("metric_type", metric);
            if let Some(expr) = filter_expr {
                options = options.filter(expr);
            }
            if !out_fields.is_empty() {
                options = options.output_fields(out_fields.clone());
            }
            let results = client
                .search(coll, vec![Value::from(q)], Some(options))
                .await?;
            Ok(result_to_hits(results.into_iter().next(), &out_fields))
        });
        hits.map_err(backend_err)
    }

    fn delete(&self, ref_id: u64) -> Result<bool> {
        if ref_id == 0 {
            return Err(VectorError::InvalidRefId);
        }
        let client = self.client.clone();
        let coll = self.collection.clone();
        let expr = format!("{PK_FIELD} in [{}]", ref_id as i64);
        let deleted: MilvusResult<i64> = self.block(async move {
            let res = client
                .delete(coll, &DeleteOptions::with_filter(expr))
                .await?;
            Ok(res.delete_cnt)
        });
        Ok(deleted.map_err(backend_err)? > 0)
    }

    fn has_ref(&self, ref_id: u64) -> bool {
        if ref_id == 0 {
            return false;
        }
        let client = self.client.clone();
        let coll = self.collection.clone();
        let expr = format!("{PK_FIELD} == {}", ref_id as i64);
        let found: MilvusResult<bool> = self.block(async move {
            let options = QueryOptions::new()
                .limit(1)
                .output_fields(vec![PK_FIELD.to_string()]);
            let cols = client.query(coll, &expr, &options).await?;
            Ok(cols.iter().any(|c| c.len() > 0))
        });
        found.unwrap_or(false)
    }

    fn count(&self) -> u64 {
        let client = self.client.clone();
        let coll = self.collection.clone();
        let stats: MilvusResult<u64> = self.block(async move {
            let m = client.get_collection_stats(&coll).await?;
            Ok(m.get("row_count")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0))
        });
        stats.unwrap_or(0)
    }

    fn reconcile_fields(&self, stored: &[FieldSpec], desired: &[FieldSpec]) -> Result<()> {
        // Milvus supports online "add field" but NOT field removal or type
        // change. Compute the diff and apply the additions; surface a clear
        // error for any drop / type change (the admin must recreate the
        // collection) rather than silently leaving the schema inconsistent.
        let stored_type = |name: &str| stored.iter().find(|f| f.name == name).map(|f| f.field_type);
        let desired_type = |name: &str| {
            desired
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.field_type)
        };

        for s in stored {
            match desired_type(&s.name) {
                None => {
                    return Err(VectorError::Backend(format!(
                        "external Milvus collection '{}' cannot drop field '{}': Milvus does not \
                         support online field removal — recreate the collection via an admin \
                         migration",
                        self.collection, s.name
                    )))
                }
                Some(dt) if dt != s.field_type => {
                    return Err(VectorError::Backend(format!(
                        "external Milvus collection '{}' cannot change the type of field '{}': \
                         Milvus does not support online type changes — recreate the collection \
                         via an admin migration",
                        self.collection, s.name
                    )))
                }
                Some(_) => {}
            }
        }

        let to_add: Vec<&FieldSpec> = desired
            .iter()
            .filter(|d| stored_type(&d.name).is_none())
            .collect();
        if to_add.is_empty() {
            return Ok(());
        }
        let client = self.client.clone();
        let coll = self.collection.clone();
        // Online-added fields must be nullable (existing rows have no value).
        let fields: Vec<_> = to_add
            .iter()
            .map(|spec| scalar_field(spec).set_nullable(true))
            .collect();
        self.block(async move {
            for field in fields {
                client.add_collection_field(coll.clone(), field).await?;
            }
            Ok::<(), milvus::error::Error>(())
        })
        .map_err(backend_err)
    }

    fn save(&self) -> Result<()> {
        // Writes are flushed inline by upsert/delete; nothing buffered locally.
        Ok(())
    }

    fn dim(&self) -> u32 {
        self.dim
    }

    fn metric(&self) -> Metric {
        self.metric
    }
}
