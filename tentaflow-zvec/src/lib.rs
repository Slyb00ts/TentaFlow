// ===== File: lib.rs — safe RAII wrapper over the zvec C API =====
//
// Exposes a single-vector-field collection keyed by a u64 ref_id (stored as the
// zvec primary key), which is exactly what `VectorBackend` in tentaflow-core needs.
// Each namespace maps to one zvec collection directory on disk. Richer features
// (metadata fields, filters, hybrid/FTS) are layered on top in later work — the
// underlying C API already supports them.

use std::ffi::{c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr;

use tentaflow_zvec_sys as sys;

/// Distance metric. Mirrors `services::vector::Metric` in tentaflow-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Cosine,
    Euclidean,
    Dot,
}

impl Metric {
    fn to_zvec(self) -> sys::zvec_metric_type_t {
        let v = match self {
            Metric::Cosine => sys::ZVEC_METRIC_TYPE_COSINE,
            Metric::Euclidean => sys::ZVEC_METRIC_TYPE_L2,
            Metric::Dot => sys::ZVEC_METRIC_TYPE_IP,
        };
        v as sys::zvec_metric_type_t
    }
}

/// Type of a metadata field stored alongside a vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    Str,
    Int,
    Float,
    Bool,
}

impl FieldType {
    fn to_zvec(self) -> sys::zvec_data_type_t {
        let v = match self {
            FieldType::Str => sys::ZVEC_DATA_TYPE_STRING,
            FieldType::Int => sys::ZVEC_DATA_TYPE_INT64,
            FieldType::Float => sys::ZVEC_DATA_TYPE_DOUBLE,
            FieldType::Bool => sys::ZVEC_DATA_TYPE_BOOL,
        };
        v as sys::zvec_data_type_t
    }
}

/// A typed metadata value.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// Declaration of a metadata field in the collection schema.
#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: String,
    pub field_type: FieldType,
}

/// A named metadata value on a document.
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub value: FieldValue,
}

/// One document for a batched upsert: its primary key (`ref_id`), dense vector,
/// typed metadata fields and an optional sparse vector `(indices, values)`.
/// Borrows so the caller can build a slice without cloning chunk data.
#[derive(Debug, Clone, Copy)]
pub struct UpsertDoc<'a> {
    pub ref_id: u64,
    pub vector: &'a [f32],
    pub fields: &'a [Field],
    pub sparse: Option<(&'a [u32], &'a [f32])>,
}

#[derive(Debug, thiserror::Error)]
pub enum ZvecError {
    #[error("zvec error {code}: {message}")]
    Api { code: i32, message: String },
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("null handle returned by zvec for {0}")]
    NullHandle(&'static str),
    #[error("non-utf8 primary key returned by zvec")]
    NonUtf8Pk,
}

pub type Result<T> = std::result::Result<T, ZvecError>;

/// Name of the single dense vector field every collection carries.
const VEC_FIELD: &[u8] = b"vec\0";

/// Name of the optional sparse vector field (present only when the namespace
/// declares `sparse = true`). Used for hybrid dense+sparse search.
const SPARSE_FIELD: &[u8] = b"sparse\0";

/// How a hybrid search fuses the dense and sparse result lists. Mirrors the
/// universal `Fusion` from tentaflow-sdk-spec; the wrapper keeps its own copy to
/// stay decoupled from the protocol crate.
#[derive(Debug, Clone, Copy)]
pub enum Fusion {
    /// Reciprocal Rank Fusion with the given rank constant (60 is conventional).
    Rrf(u32),
    /// Weighted sum of the dense and sparse scores.
    Weighted { dense: f32, sparse: f32 },
}

/// Pull the last error message zvec recorded for the current thread.
fn last_error_message() -> String {
    unsafe {
        let mut msg: *mut std::os::raw::c_char = ptr::null_mut();
        let _ = sys::zvec_get_last_error(&mut msg);
        if msg.is_null() {
            return String::from("(no message)");
        }
        let s = CStr::from_ptr(msg).to_string_lossy().into_owned();
        sys::zvec_free(msg as *mut c_void);
        s
    }
}

/// Map a zvec error code to a `Result`, attaching the last error message.
fn check(code: sys::zvec_error_code_t) -> Result<()> {
    if code == sys::zvec_error_code_t_ZVEC_OK {
        Ok(())
    } else {
        Err(ZvecError::Api {
            code: code as i32,
            message: last_error_message(),
        })
    }
}

/// Read one metadata field value from a result doc by name + declared type.
/// Returns `None` if the field is absent on this doc.
unsafe fn read_field(doc: *const sys::zvec_doc_t, name: &str, ft: FieldType) -> Option<FieldValue> {
    let cname = CString::new(name).ok()?;
    let ok = sys::zvec_error_code_t_ZVEC_OK;
    match ft {
        FieldType::Int => {
            let mut v: i64 = 0;
            let rc = sys::zvec_doc_get_field_value_basic(
                doc, cname.as_ptr(), ft.to_zvec(),
                &mut v as *mut i64 as *mut c_void, std::mem::size_of::<i64>());
            (rc == ok).then_some(FieldValue::Int(v))
        }
        FieldType::Float => {
            let mut v: f64 = 0.0;
            let rc = sys::zvec_doc_get_field_value_basic(
                doc, cname.as_ptr(), ft.to_zvec(),
                &mut v as *mut f64 as *mut c_void, std::mem::size_of::<f64>());
            (rc == ok).then_some(FieldValue::Float(v))
        }
        FieldType::Bool => {
            let mut v: u8 = 0;
            let rc = sys::zvec_doc_get_field_value_basic(
                doc, cname.as_ptr(), ft.to_zvec(),
                &mut v as *mut u8 as *mut c_void, std::mem::size_of::<u8>());
            (rc == ok).then_some(FieldValue::Bool(v != 0))
        }
        FieldType::Str => {
            let mut value: *mut c_void = ptr::null_mut();
            let mut size: usize = 0;
            let rc = sys::zvec_doc_get_field_value_copy(
                doc, cname.as_ptr(), ft.to_zvec(), &mut value, &mut size);
            if rc == ok && !value.is_null() {
                let bytes = std::slice::from_raw_parts(value as *const u8, size);
                let s = String::from_utf8_lossy(bytes).into_owned();
                sys::zvec_free(value);
                Some(FieldValue::Str(s))
            } else {
                None
            }
        }
    }
}

/// Read the "sparse" field of a result doc as `(indices, values)`. The stored
/// layout is `[nnz: u32][indices: u32*nnz][values: f32*nnz]` (native LE).
/// Returns `None` when the field is absent/empty on this doc.
unsafe fn read_sparse_field(doc: *const sys::zvec_doc_t) -> Option<(Vec<u32>, Vec<f32>)> {
    let cname = CString::new("sparse").ok()?;
    let mut value: *mut c_void = ptr::null_mut();
    let mut size: usize = 0;
    let rc = sys::zvec_doc_get_field_value_copy(
        doc,
        cname.as_ptr(),
        sys::ZVEC_DATA_TYPE_SPARSE_VECTOR_FP32 as sys::zvec_data_type_t,
        &mut value,
        &mut size,
    );
    if rc != sys::zvec_error_code_t_ZVEC_OK || value.is_null() || size < 4 {
        if !value.is_null() {
            sys::zvec_free(value);
        }
        return None;
    }
    let bytes = std::slice::from_raw_parts(value as *const u8, size);
    let nnz = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let need = 4 + nnz * 8;
    if size < need {
        sys::zvec_free(value);
        return None;
    }
    let mut indices = Vec::with_capacity(nnz);
    let mut values = Vec::with_capacity(nnz);
    let idx_off = 4;
    let val_off = 4 + nnz * 4;
    for i in 0..nnz {
        let o = idx_off + i * 4;
        indices.push(u32::from_le_bytes([
            bytes[o],
            bytes[o + 1],
            bytes[o + 2],
            bytes[o + 3],
        ]));
        let p = val_off + i * 4;
        values.push(f32::from_le_bytes([
            bytes[p],
            bytes[p + 1],
            bytes[p + 2],
            bytes[p + 3],
        ]));
    }
    sys::zvec_free(value);
    Some((indices, values))
}

/// One k-NN hit: the stored ref_id, the raw metric distance (lower = closer),
/// and any requested metadata fields.
#[derive(Debug, Clone)]
pub struct Hit {
    pub ref_id: u64,
    pub score: f32,
    pub fields: Vec<Field>,
}

/// An open zvec collection backing one vector namespace.
///
/// Not `Sync` — zvec allows single-writer access; callers that share it across
/// threads must serialize writes (tentaflow-core wraps it in a `Mutex`).
pub struct Collection {
    handle: *mut sys::zvec_collection_t,
    // The schema must outlive the collection (it owns the field schemas).
    schema: *mut sys::zvec_collection_schema_t,
    dim: u32,
    metric: Metric,
    // Declared metadata fields (name -> type), needed to read output fields back
    // with the correct getter.
    field_defs: Vec<FieldDef>,
    // Collection directory on disk — kept so a schema rebuild can swap it.
    path: PathBuf,
    // Whether this collection carries a sparse vector field (hybrid search).
    has_sparse: bool,
}

// The handle is a private heap object; moving it between threads is sound as
// long as access is externally serialized (enforced by the backend's Mutex).
unsafe impl Send for Collection {}

impl Collection {
    /// Open the collection at `path`, creating it (with an HNSW index over a
    /// single `dim`-wide FP32 vector field plus the declared metadata `fields`)
    /// if it does not exist yet. On reopen, `fields` must list the same fields so
    /// output values can be read back with the right type.
    pub fn create_or_open(
        path: &Path,
        dim: u32,
        metric: Metric,
        fields: &[FieldDef],
        sparse: bool,
    ) -> Result<Self> {
        if dim == 0 {
            return Err(ZvecError::InvalidArgument("dim must be > 0".into()));
        }
        let path_c = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| ZvecError::InvalidArgument("path contains NUL".into()))?;
        // zvec validates the collection name against `^[a-zA-Z0-9_-]{3,64}$`.
        // Derive it from the directory stem (dropping any `.ext`), replace
        // disallowed bytes (e.g. a leftover `.`) with `_`, and pad to the
        // 3-char minimum so short namespace names are still accepted.
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("namespace");
        let mut name: String = stem
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        while name.len() < 3 {
            name.push('_');
        }
        if name.len() > 64 {
            name.truncate(64);
        }
        let name_c = CString::new(name)
            .map_err(|_| ZvecError::InvalidArgument("name contains NUL".into()))?;

        // If the directory already holds a collection, open it (don't recreate —
        // create_and_open would start a fresh, empty one).
        unsafe {
            let options = sys::zvec_collection_options_create();
            if !options.is_null() {
                let _ = sys::zvec_collection_options_set_enable_mmap(options, true);
            }
            let mut handle: *mut sys::zvec_collection_t = ptr::null_mut();
            let rc = sys::zvec_collection_open(path_c.as_ptr(), options, &mut handle);
            if !options.is_null() {
                sys::zvec_collection_options_destroy(options);
            }
            if rc == sys::zvec_error_code_t_ZVEC_OK && !handle.is_null() {
                return Ok(Collection {
                    handle,
                    schema: ptr::null_mut(),
                    dim,
                    metric,
                    field_defs: fields.to_vec(),
                    path: path.to_path_buf(),
                    has_sparse: sparse,
                });
            }
            // Not openable (does not exist yet) — fall through to create it.
            sys::zvec_clear_error();
        }

        unsafe {
            let schema = sys::zvec_collection_schema_create(name_c.as_ptr());
            if schema.is_null() {
                return Err(ZvecError::NullHandle("collection_schema_create"));
            }

            // HNSW index over the vector field, parameterised by the metric.
            let index = sys::zvec_index_params_create(
                sys::ZVEC_INDEX_TYPE_HNSW as sys::zvec_index_type_t,
            );
            if index.is_null() {
                sys::zvec_collection_schema_destroy(schema);
                return Err(ZvecError::NullHandle("index_params_create"));
            }
            let r = sys::zvec_index_params_set_metric_type(index, metric.to_zvec());
            if let Err(e) = check(r) {
                sys::zvec_collection_schema_destroy(schema);
                return Err(e);
            }
            // M=32, ef_construction=200 — same defaults as zvec's own examples.
            let _ = sys::zvec_index_params_set_hnsw_params(index, 32, 200);

            let vec_field = sys::zvec_field_schema_create(
                VEC_FIELD.as_ptr() as *const std::os::raw::c_char,
                sys::ZVEC_DATA_TYPE_VECTOR_FP32 as sys::zvec_data_type_t,
                false,
                dim,
            );
            if vec_field.is_null() {
                sys::zvec_collection_schema_destroy(schema);
                return Err(ZvecError::NullHandle("field_schema_create"));
            }
            let _ = sys::zvec_field_schema_set_index_params(vec_field, index);
            let r = sys::zvec_collection_schema_add_field(schema, vec_field);
            if let Err(e) = check(r) {
                sys::zvec_collection_schema_destroy(schema);
                return Err(e);
            }

            // Metadata fields: nullable scalar columns (filterable without a
            // vector index). Keep the CStrings alive until create_and_open runs.
            let mut field_names: Vec<CString> = Vec::with_capacity(fields.len());
            for fd in fields {
                let fname = match CString::new(fd.name.as_str()) {
                    Ok(c) => c,
                    Err(_) => {
                        sys::zvec_collection_schema_destroy(schema);
                        return Err(ZvecError::InvalidArgument("field name contains NUL".into()));
                    }
                };
                let fs = sys::zvec_field_schema_create(
                    fname.as_ptr(),
                    fd.field_type.to_zvec(),
                    true,
                    0,
                );
                if fs.is_null() {
                    sys::zvec_collection_schema_destroy(schema);
                    return Err(ZvecError::NullHandle("field_schema_create(metadata)"));
                }
                if let Err(e) = check(sys::zvec_collection_schema_add_field(schema, fs)) {
                    sys::zvec_collection_schema_destroy(schema);
                    return Err(e);
                }
                field_names.push(fname);
            }

            // Optional sparse vector field for hybrid search. zvec indexes sparse
            // vectors with HNSW; with a sparse field it internally selects the
            // "InnerProductSparse" metric when the metric is set to IP.
            if sparse {
                let sp_index = sys::zvec_index_params_create(
                    sys::ZVEC_INDEX_TYPE_HNSW as sys::zvec_index_type_t,
                );
                if sp_index.is_null() {
                    sys::zvec_collection_schema_destroy(schema);
                    return Err(ZvecError::NullHandle("index_params_create(sparse)"));
                }
                let _ = sys::zvec_index_params_set_metric_type(
                    sp_index,
                    sys::ZVEC_METRIC_TYPE_IP as sys::zvec_metric_type_t,
                );
                let _ = sys::zvec_index_params_set_hnsw_params(sp_index, 32, 200);
                let sp_field = sys::zvec_field_schema_create(
                    SPARSE_FIELD.as_ptr() as *const std::os::raw::c_char,
                    sys::ZVEC_DATA_TYPE_SPARSE_VECTOR_FP32 as sys::zvec_data_type_t,
                    false,
                    0,
                );
                if sp_field.is_null() {
                    sys::zvec_collection_schema_destroy(schema);
                    return Err(ZvecError::NullHandle("field_schema_create(sparse)"));
                }
                let _ = sys::zvec_field_schema_set_index_params(sp_field, sp_index);
                if let Err(e) = check(sys::zvec_collection_schema_add_field(schema, sp_field)) {
                    sys::zvec_collection_schema_destroy(schema);
                    return Err(e);
                }
            }

            let options = sys::zvec_collection_options_create();
            if !options.is_null() {
                let _ = sys::zvec_collection_options_set_enable_mmap(options, true);
            }

            let mut handle: *mut sys::zvec_collection_t = ptr::null_mut();
            let r = sys::zvec_collection_create_and_open(
                path_c.as_ptr(),
                schema,
                options,
                &mut handle,
            );
            if !options.is_null() {
                sys::zvec_collection_options_destroy(options);
            }
            if let Err(e) = check(r) {
                sys::zvec_collection_schema_destroy(schema);
                return Err(e);
            }
            if handle.is_null() {
                sys::zvec_collection_schema_destroy(schema);
                return Err(ZvecError::NullHandle("collection_create_and_open"));
            }

            Ok(Collection {
                handle,
                schema,
                dim,
                metric,
                field_defs: fields.to_vec(),
                path: path.to_path_buf(),
                has_sparse: sparse,
            })
        }
    }

    pub fn dim(&self) -> u32 {
        self.dim
    }
    pub fn metric(&self) -> Metric {
        self.metric
    }

    /// Validate one document's vector/sparse against the collection schema.
    /// Returns the pk `CString` to reuse for replace + doc build.
    fn validate_upsert(
        &self,
        ref_id: u64,
        vector: &[f32],
        sparse: Option<(&[u32], &[f32])>,
    ) -> Result<CString> {
        if vector.len() != self.dim as usize {
            return Err(ZvecError::InvalidArgument(format!(
                "vector len {} != dim {}",
                vector.len(),
                self.dim
            )));
        }
        if let Some((indices, values)) = sparse {
            if !self.has_sparse {
                return Err(ZvecError::InvalidArgument(
                    "sparse vector supplied but collection has no sparse field".into(),
                ));
            }
            if indices.len() != values.len() {
                return Err(ZvecError::InvalidArgument(
                    "sparse indices and values length mismatch".into(),
                ));
            }
        }
        CString::new(ref_id.to_string())
            .map_err(|_| ZvecError::InvalidArgument("ref_id produced NUL".into()))
    }

    /// Build a fully populated `zvec_doc_t` (pk + dense vector + metadata +
    /// optional sparse). On any failure destroys the partial doc and returns
    /// Err, so the caller never leaks. Assumes [`validate_upsert`] already ran.
    ///
    /// # Safety
    /// Unsafe FFI; the returned pointer must be passed to `zvec_doc_destroy`.
    unsafe fn build_doc(
        &self,
        pk: &CString,
        vector: &[f32],
        fields: &[Field],
        sparse: Option<(&[u32], &[f32])>,
    ) -> Result<*mut sys::zvec_doc_t> {
        let doc = sys::zvec_doc_create();
        if doc.is_null() {
            return Err(ZvecError::NullHandle("doc_create"));
        }
        sys::zvec_doc_set_pk(doc, pk.as_ptr());
        let r = sys::zvec_doc_add_field_by_value(
            doc,
            VEC_FIELD.as_ptr() as *const std::os::raw::c_char,
            sys::ZVEC_DATA_TYPE_VECTOR_FP32 as sys::zvec_data_type_t,
            vector.as_ptr() as *const c_void,
            std::mem::size_of_val(vector),
        );
        if let Err(err) = check(r) {
            sys::zvec_doc_destroy(doc);
            return Err(err);
        }

        // Metadata fields.
        for f in fields {
            let fname = match CString::new(f.name.as_str()) {
                Ok(c) => c,
                Err(_) => {
                    sys::zvec_doc_destroy(doc);
                    return Err(ZvecError::InvalidArgument("field name contains NUL".into()));
                }
            };
            let rc = match &f.value {
                FieldValue::Str(s) => sys::zvec_doc_add_field_by_value(
                    doc, fname.as_ptr(), FieldType::Str.to_zvec(),
                    s.as_ptr() as *const c_void, s.len(),
                ),
                FieldValue::Int(i) => sys::zvec_doc_add_field_by_value(
                    doc, fname.as_ptr(), FieldType::Int.to_zvec(),
                    i as *const i64 as *const c_void, std::mem::size_of::<i64>(),
                ),
                FieldValue::Float(d) => sys::zvec_doc_add_field_by_value(
                    doc, fname.as_ptr(), FieldType::Float.to_zvec(),
                    d as *const f64 as *const c_void, std::mem::size_of::<f64>(),
                ),
                FieldValue::Bool(b) => {
                    let bb: u8 = *b as u8;
                    sys::zvec_doc_add_field_by_value(
                        doc, fname.as_ptr(), FieldType::Bool.to_zvec(),
                        &bb as *const u8 as *const c_void, std::mem::size_of::<u8>(),
                    )
                }
            };
            if let Err(err) = check(rc) {
                sys::zvec_doc_destroy(doc);
                return Err(err);
            }
        }

        // Sparse vector: serialized as [nnz: u32][indices: u32*nnz][values: f32*nnz]
        // (native LE), the layout zvec's add_field_by_value expects for
        // SPARSE_VECTOR_FP32.
        if let Some((indices, values)) = sparse {
            let nnz = indices.len() as u32;
            let mut buf = Vec::with_capacity(4 + indices.len() * 4 + values.len() * 4);
            buf.extend_from_slice(&nnz.to_le_bytes());
            for &i in indices {
                buf.extend_from_slice(&i.to_le_bytes());
            }
            for &v in values {
                buf.extend_from_slice(&v.to_le_bytes());
            }
            let rc = sys::zvec_doc_add_field_by_value(
                doc,
                SPARSE_FIELD.as_ptr() as *const std::os::raw::c_char,
                sys::ZVEC_DATA_TYPE_SPARSE_VECTOR_FP32 as sys::zvec_data_type_t,
                buf.as_ptr() as *const c_void,
                buf.len(),
            );
            if let Err(err) = check(rc) {
                sys::zvec_doc_destroy(doc);
                return Err(err);
            }
        }

        Ok(doc)
    }

    /// Insert or replace the vector stored under `ref_id`, with optional typed
    /// metadata `fields`. Flushes so a successful return implies durability.
    pub fn upsert(
        &self,
        ref_id: u64,
        vector: &[f32],
        fields: &[Field],
        sparse: Option<(&[u32], &[f32])>,
    ) -> Result<()> {
        let pk = self.validate_upsert(ref_id, vector, sparse)?;
        unsafe {
            // Replace semantics: drop any existing row with this pk first.
            let pks = [pk.as_ptr()];
            let mut s = 0usize;
            let mut e = 0usize;
            let _ = sys::zvec_collection_delete(self.handle, pks.as_ptr(), 1, &mut s, &mut e);

            let doc = self.build_doc(&pk, vector, fields, sparse)?;
            let mut docs = [doc as *const sys::zvec_doc_t];
            let mut success = 0usize;
            let mut errors = 0usize;
            let r =
                sys::zvec_collection_insert(self.handle, docs.as_mut_ptr(), 1, &mut success, &mut errors);
            sys::zvec_doc_destroy(doc);
            check(r)?;
            if success != 1 {
                return Err(ZvecError::Api {
                    code: -1,
                    message: format!("insert reported {success} ok / {errors} failed"),
                });
            }
            check(sys::zvec_collection_flush(self.handle))
        }
    }

    /// Insert-or-replace a whole batch of documents in ONE `zvec_collection_*`
    /// round-trip: one `zvec_collection_delete(count = N)` for replace semantics,
    /// one `zvec_collection_insert(count = N)` for the whole array, then a single
    /// `flush`. zvec builds its HNSW graph far more efficiently from N docs at
    /// once than from N single-doc inserts, so document ingest uses this path.
    ///
    /// Every element is validated BEFORE any doc is built or inserted; a bad
    /// element returns Err and nothing is written. Already-built docs are
    /// destroyed on every exit path (success or error) — no leak.
    pub fn upsert_batch(&self, items: &[UpsertDoc<'_>]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }

        // Validate everything first; collect pks so we can build the doc array
        // and the delete-pk array without re-allocating the strings.
        let mut pks: Vec<CString> = Vec::with_capacity(items.len());
        for item in items {
            pks.push(self.validate_upsert(item.ref_id, item.vector, item.sparse)?);
        }

        unsafe {
            // Replace semantics for the whole batch in a single delete call.
            let pk_ptrs: Vec<*const std::os::raw::c_char> =
                pks.iter().map(|c| c.as_ptr()).collect();
            let mut del_ok = 0usize;
            let mut del_err = 0usize;
            let _ = sys::zvec_collection_delete(
                self.handle,
                pk_ptrs.as_ptr(),
                pk_ptrs.len(),
                &mut del_ok,
                &mut del_err,
            );

            // Build every doc; on failure destroy the ones already built.
            let mut docs: Vec<*mut sys::zvec_doc_t> = Vec::with_capacity(items.len());
            for (item, pk) in items.iter().zip(pks.iter()) {
                match self.build_doc(pk, item.vector, item.fields, item.sparse) {
                    Ok(doc) => docs.push(doc),
                    Err(err) => {
                        for d in &docs {
                            sys::zvec_doc_destroy(*d);
                        }
                        return Err(err);
                    }
                }
            }

            let mut doc_ptrs: Vec<*const sys::zvec_doc_t> =
                docs.iter().map(|d| *d as *const sys::zvec_doc_t).collect();
            let mut success = 0usize;
            let mut errors = 0usize;
            let r = sys::zvec_collection_insert(
                self.handle,
                doc_ptrs.as_mut_ptr(),
                doc_ptrs.len(),
                &mut success,
                &mut errors,
            );

            for d in &docs {
                sys::zvec_doc_destroy(*d);
            }

            check(r)?;
            if success != items.len() {
                return Err(ZvecError::Api {
                    code: -1,
                    message: format!(
                        "batch insert reported {success} ok / {errors} failed (expected {})",
                        items.len()
                    ),
                });
            }
            check(sys::zvec_collection_flush(self.handle))
        }
    }

    /// Top-k nearest neighbours, closest first. `filter` is a backend-native
    /// filter expression (the core translates the universal AST to it);
    /// `output_fields` lists metadata field names to return on each hit.
    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&str>,
        output_fields: &[String],
    ) -> Result<Vec<Hit>> {
        if query.len() != self.dim as usize {
            return Err(ZvecError::InvalidArgument(format!(
                "query len {} != dim {}",
                query.len(),
                self.dim
            )));
        }
        let filter_c = CString::new(filter.unwrap_or(""))
            .map_err(|_| ZvecError::InvalidArgument("filter contains NUL".into()))?;
        let out_c: Vec<CString> = output_fields
            .iter()
            .map(|s| CString::new(s.as_str()))
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| ZvecError::InvalidArgument("output field contains NUL".into()))?;
        let mut out_ptrs: Vec<*const std::os::raw::c_char> = out_c.iter().map(|c| c.as_ptr()).collect();

        unsafe {
            let q = sys::zvec_vector_query_create();
            if q.is_null() {
                return Err(ZvecError::NullHandle("vector_query_create"));
            }
            let _ = sys::zvec_vector_query_set_field_name(
                q,
                VEC_FIELD.as_ptr() as *const std::os::raw::c_char,
            );
            let _ = sys::zvec_vector_query_set_query_vector(
                q,
                query.as_ptr() as *const c_void,
                std::mem::size_of_val(query),
            );
            let _ = sys::zvec_vector_query_set_topk(q, k as i32);
            let _ = sys::zvec_vector_query_set_filter(q, filter_c.as_ptr());
            let _ = sys::zvec_vector_query_set_include_vector(q, false);
            let _ = sys::zvec_vector_query_set_include_doc_id(q, true);
            if !out_ptrs.is_empty() {
                let _ = sys::zvec_vector_query_set_output_fields(q, out_ptrs.as_mut_ptr(), out_ptrs.len());
            }

            let mut results: *mut *mut sys::zvec_doc_t = ptr::null_mut();
            let mut count: usize = 0;
            let r = sys::zvec_collection_query(self.handle, q, &mut results, &mut count);
            sys::zvec_vector_query_destroy(q);
            check(r)?;
            self.read_hits(results, count, output_fields)
        }
    }

    /// Convert a zvec result doc array into `Hit`s and free it. Shared by the
    /// dense `search` and the hybrid `hybrid_search` paths.
    unsafe fn read_hits(
        &self,
        results: *mut *mut sys::zvec_doc_t,
        count: usize,
        output_fields: &[String],
    ) -> Result<Vec<Hit>> {
        let mut hits = Vec::with_capacity(count);
        if results.is_null() {
            return Ok(hits);
        }
        let slice = std::slice::from_raw_parts(results, count);
        for &doc in slice {
            if doc.is_null() {
                continue;
            }
            let pk_ptr = sys::zvec_doc_get_pk_copy(doc);
            if pk_ptr.is_null() {
                continue;
            }
            let parsed = CStr::from_ptr(pk_ptr)
                .to_str()
                .ok()
                .and_then(|s| s.parse::<u64>().ok());
            sys::zvec_free(pk_ptr as *mut c_void);
            let ref_id = match parsed {
                Some(id) => id,
                None => {
                    sys::zvec_docs_free(results, count);
                    return Err(ZvecError::NonUtf8Pk);
                }
            };
            let score = sys::zvec_doc_get_score(doc);
            let mut fields = Vec::with_capacity(output_fields.len());
            for name in output_fields {
                if let Some(fd) = self.field_defs.iter().find(|f| &f.name == name) {
                    if let Some(value) = read_field(doc, name, fd.field_type) {
                        fields.push(Field {
                            name: name.clone(),
                            value,
                        });
                    }
                }
            }
            hits.push(Hit { ref_id, score, fields });
        }
        sys::zvec_docs_free(results, count);
        Ok(hits)
    }

    /// Hybrid dense + sparse search fused with `fusion`. Runs a zvec multi-query
    /// with a dense sub-query (over the `vec` field) and a sparse sub-query (over
    /// the `sparse` field), reranked by RRF or a weighted combination. Requires
    /// the collection to have been created with `sparse = true`. `filter` and
    /// `output_fields` behave as in [`search`](Self::search).
    pub fn hybrid_search(
        &self,
        dense: &[f32],
        sparse_indices: &[u32],
        sparse_values: &[f32],
        k: usize,
        filter: Option<&str>,
        output_fields: &[String],
        fusion: Fusion,
    ) -> Result<Vec<Hit>> {
        if !self.has_sparse {
            return Err(ZvecError::InvalidArgument(
                "namespace was not created with a sparse field".into(),
            ));
        }
        if dense.len() != self.dim as usize {
            return Err(ZvecError::InvalidArgument(format!(
                "dense query len {} != dim {}",
                dense.len(),
                self.dim
            )));
        }
        if sparse_indices.len() != sparse_values.len() {
            return Err(ZvecError::InvalidArgument(
                "sparse indices and values length mismatch".into(),
            ));
        }
        let filter_c = CString::new(filter.unwrap_or(""))
            .map_err(|_| ZvecError::InvalidArgument("filter contains NUL".into()))?;
        let out_c: Vec<CString> = output_fields
            .iter()
            .map(|s| CString::new(s.as_str()))
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| ZvecError::InvalidArgument("output field contains NUL".into()))?;
        let mut out_ptrs: Vec<*const std::os::raw::c_char> =
            out_c.iter().map(|c| c.as_ptr()).collect();

        unsafe {
            let mvq = sys::zvec_multi_query_create();
            if mvq.is_null() {
                return Err(ZvecError::NullHandle("multi_query_create"));
            }
            let _ = sys::zvec_multi_query_set_topk(mvq, k as i32);
            let _ = sys::zvec_multi_query_set_include_vector(mvq, false);
            let _ = sys::zvec_multi_query_set_filter(mvq, filter_c.as_ptr());
            if !out_ptrs.is_empty() {
                let _ = sys::zvec_multi_query_set_output_fields(
                    mvq,
                    out_ptrs.as_mut_ptr(),
                    out_ptrs.len(),
                );
            }

            // Dense sub-query.
            let dq = sys::zvec_sub_query_create();
            let _ = sys::zvec_sub_query_set_field_name(
                dq,
                VEC_FIELD.as_ptr() as *const std::os::raw::c_char,
            );
            let _ = sys::zvec_sub_query_set_query_vector(
                dq,
                dense.as_ptr() as *const c_void,
                std::mem::size_of_val(dense),
            );
            let _ = sys::zvec_sub_query_set_num_candidates(dq, k as i32);
            let _ = sys::zvec_multi_query_add_sub_query(mvq, dq);

            // Sparse sub-query.
            let sq = sys::zvec_sub_query_create();
            let _ = sys::zvec_sub_query_set_field_name(
                sq,
                SPARSE_FIELD.as_ptr() as *const std::os::raw::c_char,
            );
            let _ = sys::zvec_sub_query_set_sparse_vector(
                sq,
                sparse_indices.as_ptr(),
                sparse_values.as_ptr(),
                sparse_indices.len(),
            );
            let _ = sys::zvec_sub_query_set_num_candidates(sq, k as i32);
            let _ = sys::zvec_multi_query_add_sub_query(mvq, sq);

            let _ = match fusion {
                Fusion::Rrf(c) => sys::zvec_multi_query_set_rerank_rrf(mvq, c as i32),
                Fusion::Weighted { dense, sparse } => {
                    let weights = [dense as f64, sparse as f64];
                    sys::zvec_multi_query_set_rerank_weighted(mvq, weights.as_ptr(), weights.len())
                }
            };

            let mut results: *mut *mut sys::zvec_doc_t = ptr::null_mut();
            let mut count: usize = 0;
            let r = sys::zvec_collection_multi_query(self.handle, mvq, &mut results, &mut count);

            sys::zvec_sub_query_destroy(dq);
            sys::zvec_sub_query_destroy(sq);
            sys::zvec_multi_query_destroy(mvq);
            check(r)?;
            self.read_hits(results, count, output_fields)
        }
    }

    /// Remove the vector stored under `ref_id`. Returns true if it existed.
    pub fn delete(&self, ref_id: u64) -> Result<bool> {
        let pk = CString::new(ref_id.to_string()).unwrap();
        unsafe {
            let pks = [pk.as_ptr()];
            let mut success = 0usize;
            let mut errors = 0usize;
            let r = sys::zvec_collection_delete(self.handle, pks.as_ptr(), 1, &mut success, &mut errors);
            check(r)?;
            check(sys::zvec_collection_flush(self.handle))?;
            Ok(success == 1)
        }
    }

    /// True if a vector is stored under `ref_id`.
    pub fn contains(&self, ref_id: u64) -> Result<bool> {
        let pk = CString::new(ref_id.to_string()).unwrap();
        unsafe {
            let pks = [pk.as_ptr()];
            let mut results: *mut *mut sys::zvec_doc_t = ptr::null_mut();
            let mut found: usize = 0;
            let r = sys::zvec_collection_fetch(
                self.handle,
                pks.as_ptr(),
                1,
                ptr::null(),
                0,
                false,
                &mut results,
                &mut found,
            );
            check(r)?;
            if !results.is_null() {
                sys::zvec_docs_free(results, found);
            }
            Ok(found > 0)
        }
    }

    /// Flush buffered writes to disk.
    pub fn flush(&self) -> Result<()> {
        unsafe { check(sys::zvec_collection_flush(self.handle)) }
    }

    /// The metadata fields currently declared on this collection.
    pub fn field_defs(&self) -> &[FieldDef] {
        &self.field_defs
    }

    /// Read every stored document back out (ref_id + raw vector + all currently
    /// declared metadata values). Used by [`rebuild`](Self::rebuild). zvec has
    /// no scalar-only scan, so this is a vector query with `topk = count` and a
    /// high `ef` over a zero query vector — exhaustive for the namespace sizes a
    /// schema migration realistically runs against.
    #[allow(clippy::type_complexity)]
    fn scan_all(&self) -> Result<Vec<(u64, Vec<f32>, Vec<Field>, Option<(Vec<u32>, Vec<f32>)>)>> {
        let count = self.count()? as usize;
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut out_c: Vec<CString> = self
            .field_defs
            .iter()
            .map(|f| CString::new(f.name.as_str()))
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| ZvecError::InvalidArgument("field name contains NUL".into()))?;
        // Request the sparse field too so it survives a rebuild.
        if self.has_sparse {
            out_c.push(CString::new("sparse").unwrap());
        }
        let mut out_ptrs: Vec<*const std::os::raw::c_char> =
            out_c.iter().map(|c| c.as_ptr()).collect();
        let zero = vec![0.0f32; self.dim as usize];
        let ef = count.clamp(64, 1 << 18) as i32;

        unsafe {
            let q = sys::zvec_vector_query_create();
            if q.is_null() {
                return Err(ZvecError::NullHandle("vector_query_create"));
            }
            let _ = sys::zvec_vector_query_set_field_name(
                q,
                VEC_FIELD.as_ptr() as *const std::os::raw::c_char,
            );
            let _ = sys::zvec_vector_query_set_query_vector(
                q,
                zero.as_ptr() as *const c_void,
                std::mem::size_of_val(zero.as_slice()),
            );
            let _ = sys::zvec_vector_query_set_topk(q, count as i32);
            let _ = sys::zvec_vector_query_set_include_vector(q, true);
            let _ = sys::zvec_vector_query_set_include_doc_id(q, true);
            if !out_ptrs.is_empty() {
                let _ =
                    sys::zvec_vector_query_set_output_fields(q, out_ptrs.as_mut_ptr(), out_ptrs.len());
            }
            // is_linear = true => exhaustive brute-force scan, so topk == count
            // returns every document (the HNSW graph can otherwise drop tails).
            let hp = sys::zvec_query_params_hnsw_create(ef, 0.0, true, false);
            if !hp.is_null() {
                let _ = sys::zvec_vector_query_set_hnsw_params(q, hp);
            }

            let mut results: *mut *mut sys::zvec_doc_t = ptr::null_mut();
            let mut got: usize = 0;
            let r = sys::zvec_collection_query(self.handle, q, &mut results, &mut got);
            sys::zvec_vector_query_destroy(q);
            check(r)?;

            let mut docs = Vec::with_capacity(got);
            if !results.is_null() {
                let slice = std::slice::from_raw_parts(results, got);
                for &doc in slice {
                    if doc.is_null() {
                        continue;
                    }
                    let pk_ptr = sys::zvec_doc_get_pk_copy(doc);
                    if pk_ptr.is_null() {
                        continue;
                    }
                    let parsed = CStr::from_ptr(pk_ptr)
                        .to_str()
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok());
                    sys::zvec_free(pk_ptr as *mut c_void);
                    let Some(ref_id) = parsed else {
                        sys::zvec_docs_free(results, got);
                        return Err(ZvecError::NonUtf8Pk);
                    };

                    // Vector field comes back as a copied byte buffer of LE f32.
                    let mut vptr: *mut c_void = ptr::null_mut();
                    let mut vsize: usize = 0;
                    let vrc = sys::zvec_doc_get_field_value_copy(
                        doc,
                        VEC_FIELD.as_ptr() as *const std::os::raw::c_char,
                        sys::ZVEC_DATA_TYPE_VECTOR_FP32 as sys::zvec_data_type_t,
                        &mut vptr,
                        &mut vsize,
                    );
                    let mut vector = Vec::new();
                    if vrc == sys::zvec_error_code_t_ZVEC_OK && !vptr.is_null() {
                        let bytes = std::slice::from_raw_parts(vptr as *const u8, vsize);
                        vector = bytes
                            .chunks_exact(4)
                            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                            .collect();
                        sys::zvec_free(vptr);
                    }

                    let mut fields = Vec::with_capacity(self.field_defs.len());
                    for fd in &self.field_defs {
                        if let Some(value) = read_field(doc, &fd.name, fd.field_type) {
                            fields.push(Field {
                                name: fd.name.clone(),
                                value,
                            });
                        }
                    }

                    // Sparse field (when present) comes back as the same
                    // [nnz:u32][indices:u32*nnz][values:f32*nnz] buffer we insert.
                    let sparse = if self.has_sparse {
                        read_sparse_field(doc)
                    } else {
                        None
                    };

                    docs.push((ref_id, vector, fields, sparse));
                }
                sys::zvec_docs_free(results, got);
            }
            Ok(docs)
        }
    }

    /// Rebuild this collection with a new metadata schema (reconciliation on
    /// addon update). zvec online DDL is numeric-only, so the universal path is
    /// a full rebuild: scan every document out, create a sibling collection with
    /// `new_fields`, re-insert (dropping removed fields, leaving added ones
    /// NULL), then atomically swap the directory and reopen. A no-op when the
    /// schema already matches.
    pub fn rebuild(&mut self, new_fields: &[FieldDef]) -> Result<()> {
        let same = self.field_defs.len() == new_fields.len()
            && new_fields.iter().all(|n| {
                self.field_defs
                    .iter()
                    .any(|c| c.name == n.name && c.field_type == n.field_type)
            });
        if same {
            return Ok(());
        }

        let docs = self.scan_all()?;
        let dim = self.dim;
        let metric = self.metric;
        let old_path = self.path.clone();
        let temp_path = match old_path.file_name().and_then(|s| s.to_str()) {
            Some(name) => old_path.with_file_name(format!("{name}.rebuild")),
            None => return Err(ZvecError::InvalidArgument("collection path has no name".into())),
        };
        if temp_path.exists() {
            let _ = std::fs::remove_dir_all(&temp_path);
        }

        let keep: std::collections::HashSet<&str> =
            new_fields.iter().map(|f| f.name.as_str()).collect();
        let has_sparse = self.has_sparse;
        {
            let newc = Collection::create_or_open(&temp_path, dim, metric, new_fields, has_sparse)?;
            for (ref_id, vector, fields, sparse) in &docs {
                let filtered: Vec<Field> = fields
                    .iter()
                    .filter(|f| keep.contains(f.name.as_str()))
                    .cloned()
                    .collect();
                let sparse_ref = sparse.as_ref().map(|(i, v)| (i.as_slice(), v.as_slice()));
                newc.upsert(*ref_id, vector, &filtered, sparse_ref)?;
            }
            // newc dropped here -> closed + flushed to temp_path.
        }

        // Close the live handle/schema so the directory can be replaced.
        unsafe {
            if !self.handle.is_null() {
                sys::zvec_collection_close(self.handle);
                self.handle = ptr::null_mut();
            }
            if !self.schema.is_null() {
                sys::zvec_collection_schema_destroy(self.schema);
                self.schema = ptr::null_mut();
            }
        }
        std::fs::remove_dir_all(&old_path)
            .map_err(|e| ZvecError::Api { code: -1, message: format!("rebuild remove old: {e}") })?;
        std::fs::rename(&temp_path, &old_path)
            .map_err(|e| ZvecError::Api { code: -1, message: format!("rebuild swap dir: {e}") })?;

        // Reopen with the new schema. The current `*self` now holds null handles
        // (closed above), so assigning a fresh Collection drops a harmless shell.
        *self = Collection::create_or_open(&old_path, dim, metric, new_fields, has_sparse)?;
        Ok(())
    }

    /// Current document count (authoritative, from the native index).
    pub fn count(&self) -> Result<u64> {
        unsafe {
            let mut stats: *mut sys::zvec_collection_stats_t = ptr::null_mut();
            check(sys::zvec_collection_get_stats(self.handle, &mut stats))?;
            if stats.is_null() {
                return Ok(0);
            }
            let n = sys::zvec_collection_stats_get_doc_count(stats);
            sys::zvec_collection_stats_destroy(stats);
            Ok(n as u64)
        }
    }
}

impl Drop for Collection {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_null() {
                // close() flushes to disk AND frees the handle. destroy() must NOT
                // also be called (it would double-free the closed handle → crash).
                let _ = sys::zvec_collection_close(self.handle);
            }
            if !self.schema.is_null() {
                sys::zvec_collection_schema_destroy(self.schema);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_search_delete_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns_a");
        let coll = Collection::create_or_open(&path, 4, Metric::Cosine, &[], false).unwrap();

        // Three distinct unit-ish vectors along different axes.
        coll.upsert(10, &[1.0, 0.0, 0.0, 0.0], &[], None).unwrap();
        coll.upsert(20, &[0.0, 1.0, 0.0, 0.0], &[], None).unwrap();
        coll.upsert(30, &[0.0, 0.0, 1.0, 0.0], &[], None).unwrap();
        assert_eq!(coll.count().unwrap(), 3);

        // Query closest to the first axis — id 10 must come back first.
        let hits = coll.search(&[0.9, 0.1, 0.0, 0.0], 2, None, &[]).unwrap();
        assert!(!hits.is_empty(), "expected at least one hit");
        assert_eq!(hits[0].ref_id, 10, "nearest neighbour should be id 10");

        // Delete it and confirm it's gone from the count.
        assert!(coll.delete(10).unwrap());
        assert_eq!(coll.count().unwrap(), 2);
    }

    #[test]
    fn reopen_persists_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns_persist");
        {
            let coll = Collection::create_or_open(&path, 3, Metric::Euclidean, &[], false).unwrap();
            coll.upsert(1, &[1.0, 2.0, 3.0], &[], None).unwrap();
            assert_eq!(coll.count().unwrap(), 1);
        }
        // Reopen the same directory — the vector must still be there.
        let coll = Collection::create_or_open(&path, 3, Metric::Euclidean, &[], false).unwrap();
        assert_eq!(coll.count().unwrap(), 1);
        let hits = coll.search(&[1.0, 2.0, 3.0], 1, None, &[]).unwrap();
        assert_eq!(hits[0].ref_id, 1);
    }

    #[test]
    fn upsert_batch_inserts_all_in_one_call_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns_batch");
        let fields = vec![FieldDef { name: "chunk".into(), field_type: FieldType::Int }];

        // Three distinct docs in a SINGLE batched insert (count = N).
        let vectors: Vec<Vec<f32>> = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![0.0, 0.0, 1.0],
        ];
        let chunk_fields: Vec<Vec<Field>> = (0..3)
            .map(|i| vec![Field { name: "chunk".into(), value: FieldValue::Int(i) }])
            .collect();
        {
            let coll = Collection::create_or_open(&path, 3, Metric::Cosine, &fields, false).unwrap();
            let items: Vec<UpsertDoc<'_>> = vectors
                .iter()
                .zip(chunk_fields.iter())
                .enumerate()
                .map(|(i, (v, f))| UpsertDoc {
                    ref_id: (i as u64) + 1,
                    vector: v.as_slice(),
                    fields: f.as_slice(),
                    sparse: None,
                })
                .collect();
            coll.upsert_batch(&items).unwrap();
            assert_eq!(coll.count().unwrap(), 3);
        }

        // Reopen proves the single batched insert + one flush was durable.
        let coll = Collection::create_or_open(&path, 3, Metric::Cosine, &fields, false).unwrap();
        assert_eq!(coll.count().unwrap(), 3);
        let hit = coll.search(&[0.0, 1.0, 0.0], 1, None, &["chunk".to_string()]).unwrap();
        assert_eq!(hit[0].ref_id, 2);

        // Re-batching the same ref_ids replaces (no duplicates).
        let v2 = [vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0], vec![0.0, 0.0, 1.0]];
        let f2: Vec<Vec<Field>> = (0..3)
            .map(|i| vec![Field { name: "chunk".into(), value: FieldValue::Int(i + 100) }])
            .collect();
        let items2: Vec<UpsertDoc<'_>> = v2
            .iter()
            .zip(f2.iter())
            .enumerate()
            .map(|(i, (v, f))| UpsertDoc {
                ref_id: (i as u64) + 1,
                vector: v.as_slice(),
                fields: f.as_slice(),
                sparse: None,
            })
            .collect();
        coll.upsert_batch(&items2).unwrap();
        assert_eq!(coll.count().unwrap(), 3);
    }

    #[test]
    fn upsert_batch_empty_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns_batch_empty");
        let coll = Collection::create_or_open(&path, 3, Metric::Cosine, &[], false).unwrap();
        coll.upsert_batch(&[]).unwrap();
        assert_eq!(coll.count().unwrap(), 0);
    }

    #[test]
    fn upsert_batch_rejects_bad_dim_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns_batch_dim");
        let coll = Collection::create_or_open(&path, 3, Metric::Cosine, &[], false).unwrap();
        let good = vec![1.0, 0.0, 0.0];
        let bad = vec![1.0, 0.0]; // wrong dim
        let items = vec![
            UpsertDoc { ref_id: 1, vector: good.as_slice(), fields: &[], sparse: None },
            UpsertDoc { ref_id: 2, vector: bad.as_slice(), fields: &[], sparse: None },
        ];
        assert!(coll.upsert_batch(&items).is_err());
        // Validation runs before any insert: nothing written.
        assert_eq!(coll.count().unwrap(), 0);
    }

    #[test]
    fn metadata_fields_and_filter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns_meta");
        let fields = vec![
            FieldDef { name: "source".into(), field_type: FieldType::Str },
            FieldDef { name: "score".into(), field_type: FieldType::Int },
        ];
        let coll = Collection::create_or_open(&path, 3, Metric::Cosine, &fields, false).unwrap();
        coll.upsert(1, &[1.0, 0.0, 0.0],
            &[Field { name: "source".into(), value: FieldValue::Str("web".into()) },
              Field { name: "score".into(), value: FieldValue::Int(10) }], None).unwrap();
        coll.upsert(2, &[0.9, 0.1, 0.0],
            &[Field { name: "source".into(), value: FieldValue::Str("inbox".into()) },
              Field { name: "score".into(), value: FieldValue::Int(20) }], None).unwrap();

        // bez filtra: najblizszy do osi X to id 1 lub 2; z output_fields zwracamy metadane
        let hits = coll.search(&[1.0, 0.0, 0.0], 2, None,
            &["source".to_string(), "score".to_string()]).unwrap();
        assert!(!hits.is_empty());
        assert!(hits[0].fields.iter().any(|f| f.name == "source"));

        // filtr source = 'inbox' -> tylko id 2
        let hits = coll.search(&[1.0, 0.0, 0.0], 5, Some("source = 'inbox'"),
            &["source".to_string()]).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].ref_id, 2);
        match &hits[0].fields[0].value {
            FieldValue::Str(s) => assert_eq!(s, "inbox"),
            other => panic!("oczekiwano Str, jest {other:?}"),
        }
    }

    #[test]
    fn rebuild_drops_string_adds_numeric_and_preserves_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns_rebuild");
        let mut coll = Collection::create_or_open(
            &path,
            3,
            Metric::Cosine,
            &[FieldDef { name: "source".into(), field_type: FieldType::Str }],
            false,
)
        .unwrap();
        coll.upsert(
            1,
            &[1.0, 0.0, 0.0],
            &[Field { name: "source".into(), value: FieldValue::Str("web".into()) }],
            None,
        )
        .unwrap();
        coll.upsert(
            2,
            &[0.0, 1.0, 0.0],
            &[Field { name: "source".into(), value: FieldValue::Str("inbox".into()) }],
            None,
        )
        .unwrap();

        // Drop the STRING column "source", add a numeric "score". zvec online DDL
        // cannot do this on a string column, so rebuild() recreates the
        // collection — the vectors must survive the swap.
        coll.rebuild(&[FieldDef { name: "score".into(), field_type: FieldType::Int }])
            .unwrap();
        assert!(coll.field_defs().iter().any(|f| f.name == "score"));
        assert!(!coll.field_defs().iter().any(|f| f.name == "source"));
        assert_eq!(coll.count().unwrap(), 2, "both vectors survive the rebuild");

        // The new column is usable; the preserved vectors still match by geometry.
        coll.upsert(
            1,
            &[1.0, 0.0, 0.0],
            &[Field { name: "score".into(), value: FieldValue::Int(77) }],
            None,
        )
        .unwrap();
        let hits = coll
            .search(&[1.0, 0.0, 0.0], 5, Some("score > 50"), &["score".to_string()])
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].ref_id, 1);

        // A no-op rebuild (same schema) leaves data intact.
        coll.rebuild(&[FieldDef { name: "score".into(), field_type: FieldType::Int }])
            .unwrap();
        assert_eq!(coll.count().unwrap(), 2);
    }

    #[test]
    fn hybrid_search_fuses_dense_and_sparse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns_hybrid");
        let coll = Collection::create_or_open(&path, 4, Metric::Cosine, &[], true).unwrap();

        // doc 1: dense aligned with X; sparse term 100 strong.
        coll.upsert(1, &[1.0, 0.0, 0.0, 0.0], &[], Some((&[100, 200], &[0.9, 0.1])))
            .unwrap();
        // doc 2: dense aligned with Y; sparse term 300 strong.
        coll.upsert(2, &[0.0, 1.0, 0.0, 0.0], &[], Some((&[300, 400], &[0.8, 0.2])))
            .unwrap();

        // Dense query near X (favours doc 1) + sparse query on term 300 (favours
        // doc 2). RRF fuses both; both docs should surface.
        let hits = coll
            .hybrid_search(
                &[0.9, 0.1, 0.0, 0.0],
                &[300],
                &[1.0],
                5,
                None,
                &[],
                Fusion::Rrf(60),
            )
            .unwrap();
        assert!(hits.len() >= 2, "hybrid should fuse dense + sparse candidates");
        let ids: std::collections::HashSet<u64> = hits.iter().map(|h| h.ref_id).collect();
        assert!(ids.contains(&1) && ids.contains(&2));

        // Pure-sparse intent (term 300) under weighted fusion favouring sparse:
        // doc 2 must rank first.
        let hits = coll
            .hybrid_search(
                &[0.0, 0.0, 1.0, 0.0],
                &[300],
                &[1.0],
                5,
                None,
                &[],
                Fusion::Weighted { dense: 0.1, sparse: 0.9 },
            )
            .unwrap();
        assert_eq!(hits[0].ref_id, 2);

        // Sparse on a dense-only collection is rejected.
        let dense_only = Collection::create_or_open(
            &dir.path().join("ns_dense_only"),
            4,
            Metric::Cosine,
            &[],
            false,
        )
        .unwrap();
        assert!(dense_only
            .hybrid_search(&[1.0, 0.0, 0.0, 0.0], &[1], &[1.0], 5, None, &[], Fusion::Rrf(60))
            .is_err());
    }
}
