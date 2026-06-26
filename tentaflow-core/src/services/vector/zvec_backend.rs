// ============ File: services/vector/zvec_backend.rs — embedded zvec backend ============
//
// `VectorBackend` over the embedded zvec engine (tentaflow-zvec safe wrapper).
// Each namespace is one zvec collection living in a directory on disk; the path
// the namespace manager hands us (`<...>/vectors/<ns>.usearch`) is used verbatim
// as that collection directory (zvec creates its files inside it).
//
// zvec allows concurrent readers but a single writer; the safe `Collection` is
// `Send` but not `Sync`, so we serialize every operation behind a `parking_lot`
// mutex. Writes are flushed to disk by the wrapper before they return, so a
// successful upsert/delete implies durability — same contract as before.

use std::path::PathBuf;

use parking_lot::Mutex;
use tentaflow_zvec::{
    Collection, Field as ZField, FieldDef as ZFieldDef, FieldType as ZFieldType,
    FieldValue as ZFieldValue, Fusion as ZFusion, Metric as ZMetric, UpsertDoc, ZvecError,
};

use super::backend::{
    Field, FieldSpec, FieldValue, Filter, Fusion, Metric, SearchHit, SparseVector, UpsertItem,
    VectorBackend,
};
use super::error::{Result, VectorError};
use super::filter;

pub struct ZvecBackend {
    inner: Mutex<Collection>,
    dim: u32,
    metric: Metric,
    sparse: bool,
}

fn to_zfield_type(t: tentaflow_sdk_spec::FieldType) -> ZFieldType {
    use tentaflow_sdk_spec::FieldType as T;
    match t {
        T::Str => ZFieldType::Str,
        T::Int => ZFieldType::Int,
        T::Float => ZFieldType::Float,
        T::Bool => ZFieldType::Bool,
    }
}

fn to_zvalue(v: &FieldValue) -> ZFieldValue {
    match v {
        FieldValue::Str(s) => ZFieldValue::Str(s.clone()),
        FieldValue::Int(i) => ZFieldValue::Int(*i),
        FieldValue::Float(f) => ZFieldValue::Float(*f),
        FieldValue::Bool(b) => ZFieldValue::Bool(*b),
    }
}

fn from_zvalue(v: ZFieldValue) -> FieldValue {
    match v {
        ZFieldValue::Str(s) => FieldValue::Str(s),
        ZFieldValue::Int(i) => FieldValue::Int(i),
        ZFieldValue::Float(f) => FieldValue::Float(f),
        ZFieldValue::Bool(b) => FieldValue::Bool(b),
    }
}

fn to_zvec_metric(m: Metric) -> ZMetric {
    match m {
        Metric::Cosine => ZMetric::Cosine,
        Metric::Euclidean => ZMetric::Euclidean,
        Metric::Dot => ZMetric::Dot,
    }
}

fn to_zvec_fusion(f: &Fusion) -> ZFusion {
    match f {
        Fusion::Rrf(k) => ZFusion::Rrf(*k),
        Fusion::Weighted(dense, sparse) => ZFusion::Weighted {
            dense: *dense,
            sparse: *sparse,
        },
    }
}

fn map_err(e: ZvecError) -> VectorError {
    VectorError::Backend(e.to_string())
}

/// Restrict a path to owner-only access. Embeddings of regulated data (face
/// vectors, person attributes) are PII; the default umask would leave the
/// collection directory world-readable.
fn tighten_mode(path: &std::path::Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| {
            VectorError::Io {
                path: Some(path.to_path_buf()),
                source: e,
            }
        })?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

impl ZvecBackend {
    /// Open the zvec collection at `file_path` (used as the collection
    /// directory), creating it if absent. `fields` is the declared metadata
    /// schema for the namespace.
    pub fn open_or_create(
        file_path: PathBuf,
        dim: u32,
        metric: Metric,
        fields: &[FieldSpec],
        sparse: bool,
    ) -> Result<Self> {
        if !(1..=4096).contains(&dim) {
            return Err(VectorError::InvalidDim(dim));
        }
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| VectorError::Io {
                path: Some(parent.to_path_buf()),
                source: e,
            })?;
            tighten_mode(parent, 0o700)?;
        }
        let zfields: Vec<ZFieldDef> = fields
            .iter()
            .map(|f| ZFieldDef {
                name: f.name.clone(),
                field_type: to_zfield_type(f.field_type),
            })
            .collect();
        let coll =
            Collection::create_or_open(&file_path, dim, to_zvec_metric(metric), &zfields, sparse)
                .map_err(map_err)?;
        // zvec created the collection directory — lock it down too.
        let _ = tighten_mode(&file_path, 0o700);
        Ok(Self {
            inner: Mutex::new(coll),
            dim,
            metric,
            sparse,
        })
    }
}

impl VectorBackend for ZvecBackend {
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
        let zfields: Vec<ZField> = fields
            .iter()
            .map(|f| ZField {
                name: f.name.clone(),
                value: to_zvalue(&f.value),
            })
            .collect();
        let sparse_ref = sparse.map(|s| (s.indices.as_slice(), s.values.as_slice()));
        self.inner
            .lock()
            .upsert(ref_id, vector, &zfields, sparse_ref)
            .map_err(map_err)
    }

    fn upsert_batch(&self, items: &[UpsertItem<'_>]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        // Validate every item up front and materialize owned zvec field vectors
        // that outlive the borrowing `UpsertDoc` slice handed to the wrapper.
        let mut zfields_all: Vec<Vec<ZField>> = Vec::with_capacity(items.len());
        for item in items {
            if item.ref_id == 0 {
                return Err(VectorError::InvalidRefId);
            }
            if item.vector.is_empty() {
                return Err(VectorError::EmptyVector);
            }
            if item.vector.len() as u32 != self.dim {
                return Err(VectorError::DimMismatch {
                    expected: self.dim,
                    actual: item.vector.len() as u32,
                });
            }
            if item.sparse.is_some() && !self.sparse {
                return Err(VectorError::Backend(
                    "namespace does not support sparse vectors (declare sparse = true)".into(),
                ));
            }
            zfields_all.push(
                item.fields
                    .iter()
                    .map(|f| ZField {
                        name: f.name.clone(),
                        value: to_zvalue(&f.value),
                    })
                    .collect(),
            );
        }

        let docs: Vec<UpsertDoc<'_>> = items
            .iter()
            .zip(zfields_all.iter())
            .map(|(item, zfields)| UpsertDoc {
                ref_id: item.ref_id,
                vector: item.vector,
                fields: zfields.as_slice(),
                sparse: item
                    .sparse
                    .map(|s| (s.indices.as_slice(), s.values.as_slice())),
            })
            .collect();

        self.inner.lock().upsert_batch(&docs).map_err(map_err)
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
        let filter_str = match filter {
            Some(f) => Some(filter::to_zvec(f)?),
            None => None,
        };
        let hits = self
            .inner
            .lock()
            .hybrid_search(
                dense,
                &sparse.indices,
                &sparse.values,
                k,
                filter_str.as_deref(),
                output_fields,
                to_zvec_fusion(&fusion),
            )
            .map_err(map_err)?;
        Ok(hits
            .into_iter()
            .map(|h| SearchHit {
                ref_id: h.ref_id,
                score: h.score,
                fields: h
                    .fields
                    .into_iter()
                    .map(|f| Field {
                        name: f.name,
                        value: from_zvalue(f.value),
                    })
                    .collect(),
            })
            .collect())
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
        let filter_str = match filter {
            Some(f) => Some(filter::to_zvec(f)?),
            None => None,
        };
        let hits = self
            .inner
            .lock()
            .search(query, k, filter_str.as_deref(), output_fields)
            .map_err(map_err)?;
        Ok(hits
            .into_iter()
            .map(|h| SearchHit {
                ref_id: h.ref_id,
                score: h.score,
                fields: h
                    .fields
                    .into_iter()
                    .map(|f| Field {
                        name: f.name,
                        value: from_zvalue(f.value),
                    })
                    .collect(),
            })
            .collect())
    }

    fn delete(&self, ref_id: u64) -> Result<bool> {
        if ref_id == 0 {
            return Err(VectorError::InvalidRefId);
        }
        self.inner.lock().delete(ref_id).map_err(map_err)
    }

    fn has_ref(&self, ref_id: u64) -> bool {
        if ref_id == 0 {
            return false;
        }
        self.inner.lock().contains(ref_id).unwrap_or(false)
    }

    fn count(&self) -> u64 {
        self.inner.lock().count().unwrap_or(0)
    }

    fn save(&self) -> Result<()> {
        self.inner.lock().flush().map_err(map_err)
    }

    fn reconcile_fields(&self, _stored: &[FieldSpec], desired: &[FieldSpec]) -> Result<()> {
        // zvec online DDL is numeric-only, so the universal path is a full
        // rebuild; the wrapper short-circuits to a no-op when the schema already
        // matches its live `field_defs`.
        let zdesired: Vec<ZFieldDef> = desired
            .iter()
            .map(|f| ZFieldDef {
                name: f.name.clone(),
                field_type: to_zfield_type(f.field_type),
            })
            .collect();
        self.inner.lock().rebuild(&zdesired).map_err(map_err)
    }

    fn dim(&self) -> u32 {
        self.dim
    }

    fn metric(&self) -> Metric {
        self.metric
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn tmp_backend(dim: u32, metric: Metric) -> (TempDir, Arc<ZvecBackend>) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ns.usearch");
        let be = ZvecBackend::open_or_create(path, dim, metric, &[], false).unwrap();
        (dir, Arc::new(be))
    }

    #[test]
    fn test_open_create_and_upsert() {
        let (_dir, be) = tmp_backend(4, Metric::Cosine);
        assert_eq!(be.count(), 0);
        be.upsert(1, &[1.0, 0.0, 0.0, 0.0], &[], None).unwrap();
        be.upsert(2, &[0.0, 1.0, 0.0, 0.0], &[], None).unwrap();
        assert_eq!(be.count(), 2);
    }

    #[test]
    fn test_search_returns_nearest_top_k() {
        let (_dir, be) = tmp_backend(4, Metric::Cosine);
        be.upsert(10, &[1.0, 0.0, 0.0, 0.0], &[], None).unwrap();
        be.upsert(20, &[0.0, 1.0, 0.0, 0.0], &[], None).unwrap();
        be.upsert(30, &[0.0, 0.0, 1.0, 0.0], &[], None).unwrap();
        let hits = be.search(&[0.99, 0.01, 0.0, 0.0], 2, None, &[]).unwrap();
        assert_eq!(hits[0].ref_id, 10);
    }

    #[test]
    fn test_delete_and_has_ref() {
        let (_dir, be) = tmp_backend(3, Metric::Cosine);
        be.upsert(1, &[1.0, 0.0, 0.0], &[], None).unwrap();
        assert!(be.has_ref(1));
        assert!(be.delete(1).unwrap());
        assert!(!be.has_ref(1));
    }

    #[test]
    fn test_dim_mismatch_rejected() {
        let (_dir, be) = tmp_backend(4, Metric::Cosine);
        assert!(matches!(
            be.upsert(1, &[1.0, 0.0], &[], None).unwrap_err(),
            VectorError::DimMismatch { .. }
        ));
    }

    #[test]
    fn test_invalid_ref_id_rejected() {
        let (_dir, be) = tmp_backend(2, Metric::Cosine);
        assert!(matches!(
            be.upsert(0, &[1.0, 0.0], &[], None).unwrap_err(),
            VectorError::InvalidRefId
        ));
    }

    #[test]
    fn test_persist_and_reopen() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("persist.usearch");
        {
            let be =
                ZvecBackend::open_or_create(path.clone(), 3, Metric::Cosine, &[], false).unwrap();
            be.upsert(7, &[1.0, 0.0, 0.0], &[], None).unwrap();
            be.upsert(8, &[0.0, 1.0, 0.0], &[], None).unwrap();
        }
        let be2 = ZvecBackend::open_or_create(path, 3, Metric::Cosine, &[], false).unwrap();
        assert_eq!(be2.count(), 2);
        let hits = be2.search(&[1.0, 0.0, 0.0], 1, None, &[]).unwrap();
        assert_eq!(hits[0].ref_id, 7);
    }

    #[test]
    fn fields_and_filter_through_trait() {
        use tentaflow_sdk_spec::{Field, FieldSpec, FieldType, FieldValue, Filter};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ns.usearch");
        let schema = vec![FieldSpec {
            name: "source".into(),
            field_type: FieldType::Str,
            indexed: true,
        }];
        let be = ZvecBackend::open_or_create(path, 3, Metric::Cosine, &schema, false).unwrap();
        be.upsert(
            1,
            &[1.0, 0.0, 0.0],
            &[Field {
                name: "source".into(),
                value: FieldValue::Str("web".into()),
            }],
            None,
        )
        .unwrap();
        be.upsert(
            2,
            &[0.9, 0.1, 0.0],
            &[Field {
                name: "source".into(),
                value: FieldValue::Str("inbox".into()),
            }],
            None,
        )
        .unwrap();
        let f = Filter::Eq("source".into(), FieldValue::Str("inbox".into()));
        let hits = be
            .search(&[1.0, 0.0, 0.0], 5, Some(&f), &["source".to_string()])
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].ref_id, 2);
        assert!(matches!(&hits[0].fields[0].value, FieldValue::Str(s) if s == "inbox"));
    }
}
