// ===== File: services/vector/doc_vectors.rs — document-scoped vector primitives =====
//
// One implementation of the three operations every "store the chunks of a
// document" path needs: derive a stable ref id, wipe a document's previous
// vectors before rewriting it, and roll a failed batch back.
//
// They used to exist twice — in the `store` flow node and in the Project Studio
// ingest job — and the copies had already drifted: the node cleaned up in a
// single pass while ingest looped until a pass came back empty. Divergence like
// that is the reason this module exists; the retrieval quality of both consumers
// depends on cleanup actually being complete.

use super::backend::VectorBackend;
use super::error::Result;
use tentaflow_sdk_spec::{FieldValue, Filter};

/// Cap on the cleanup query. One document never has more chunks than this, so a
/// filtered search with this `k` returns all of them.
pub const CLEANUP_SEARCH_K: usize = 100_000;

/// Upper bound on repeated cleanup passes (see [`delete_doc_vectors`]); a
/// well-behaved index empties in one or two.
pub const CLEANUP_MAX_PASSES: usize = 8;

/// Deterministic ref id from `(doc_id, chunk_index)` — FNV-1a 64-bit over the
/// doc id, mixed with the chunk index.
///
/// Determinism is what makes a re-ingest REPLACE a chunk's vector instead of
/// duplicating it. ref_id 0 is reserved by the zvec backend, so it is forced to
/// a non-zero value.
pub fn ref_id_for(doc_id: &str, chunk_index: u64) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in doc_id.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash ^= chunk_index.wrapping_add(1);
    hash = hash.wrapping_mul(0x100000001b3);
    if hash == 0 {
        1
    } else {
        hash
    }
}

/// Removes every vector of `doc_id` from `backend`.
///
/// A stable ref id only overwrites chunks at the SAME index, so a re-ingest that
/// produces fewer chunks would strand the tail — hence a delete-by-document pass
/// before every write.
///
/// The backend exposes no delete-by-filter and no full scan, so cleanup rides on
/// filtered ANN searches. A single pass can under-report (HNSW recall is
/// approximate, markedly so with the zero-vector probe the delete-only callers
/// have to use), therefore search+delete repeats until a pass finds nothing —
/// deleting the hits improves reachability of the remainder — bounded by
/// [`CLEANUP_MAX_PASSES`].
///
/// `probe` should be a real vector of the document being written whenever the
/// caller has one: it lands near the old vectors and maximises recall. `None`
/// falls back to the zero vector, which degenerates under cosine.
pub fn delete_doc_vectors(
    backend: &dyn VectorBackend,
    doc_id: &str,
    probe: Option<&[f32]>,
) -> Result<()> {
    let filter = Filter::Eq("doc_id".to_string(), FieldValue::Str(doc_id.to_string()));
    let zero;
    let probe: &[f32] = match probe {
        Some(p) => p,
        None => {
            zero = vec![0.0f32; backend.dim() as usize];
            zero.as_slice()
        }
    };
    for _ in 0..CLEANUP_MAX_PASSES {
        let existing = backend.search(probe, CLEANUP_SEARCH_K, Some(&filter), &[])?;
        if existing.is_empty() {
            return Ok(());
        }
        for hit in existing {
            backend.delete(hit.ref_id)?;
        }
    }
    Ok(())
}

/// Best-effort removal of `refs` after a failed write, so a partially applied
/// batch does not survive as unreachable half-document. Every ref is attempted;
/// the FIRST failure is returned for the caller to append to its own error.
pub fn rollback_refs(backend: &dyn VectorBackend, refs: &[u64]) -> Option<String> {
    let mut first_err: Option<String> = None;
    for r in refs {
        if let Err(e) = backend.delete(*r) {
            first_err.get_or_insert(format!("ref {r}: {e}"));
        }
    }
    first_err
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::vector::backend::{Fusion, Metric, SearchHit, SparseVector};
    use crate::services::vector::error::VectorError;
    use std::sync::Mutex;
    use tentaflow_sdk_spec::{Field, FieldSpec};

    /// Backend, ktory zwraca tylko CZESC pasujacych wektorow na przebieg —
    /// odwzorowuje przyblizony recall filtrowanego ANN. Jednoprzebiegowy cleanup
    /// zostawilby na nim orphany.
    struct PartialRecallBackend {
        /// Kolejne "strony" wynikow; kazdy `search` zdejmuje jedna.
        pages: Mutex<Vec<Vec<u64>>>,
        deleted: Mutex<Vec<u64>>,
        searches: Mutex<usize>,
    }

    impl VectorBackend for PartialRecallBackend {
        fn upsert(
            &self,
            _ref_id: u64,
            _vector: &[f32],
            _fields: &[Field],
            _sparse: Option<&SparseVector>,
        ) -> Result<()> {
            Ok(())
        }
        fn search(
            &self,
            _query: &[f32],
            _k: usize,
            _filter: Option<&Filter>,
            _output_fields: &[String],
        ) -> Result<Vec<SearchHit>> {
            *self.searches.lock().unwrap() += 1;
            let mut pages = self.pages.lock().unwrap();
            if pages.is_empty() {
                return Ok(Vec::new());
            }
            let page = pages.remove(0);
            Ok(page
                .into_iter()
                .map(|ref_id| SearchHit {
                    ref_id,
                    score: 1.0,
                    fields: Vec::new(),
                })
                .collect())
        }
        fn hybrid_search(
            &self,
            _dense: &[f32],
            _sparse: &SparseVector,
            _k: usize,
            _filter: Option<&Filter>,
            _output_fields: &[String],
            _fusion: Fusion,
        ) -> Result<Vec<SearchHit>> {
            Err(VectorError::Backend("not used".into()))
        }
        fn delete(&self, ref_id: u64) -> Result<bool> {
            self.deleted.lock().unwrap().push(ref_id);
            Ok(true)
        }
        fn has_ref(&self, _ref_id: u64) -> bool {
            false
        }
        fn count(&self) -> u64 {
            0
        }
        fn save(&self) -> Result<()> {
            Ok(())
        }
        fn reconcile_fields(&self, _stored: &[FieldSpec], _desired: &[FieldSpec]) -> Result<()> {
            Ok(())
        }
        fn dim(&self) -> u32 {
            3
        }
        fn metric(&self) -> Metric {
            Metric::Cosine
        }
    }

    /// Cleanup powtarza search+delete az przebieg nic nie znajdzie. To jest
    /// wlasnie zachowanie, ktorego brakowalo kopii w nodzie `store`: przy
    /// czesciowym recall jeden przebieg zostawia stare wektory dokumentu, a te
    /// wracaja pozniej w wynikach wyszukiwania.
    #[test]
    fn cleanup_repeats_until_a_pass_finds_nothing() {
        let backend = PartialRecallBackend {
            pages: Mutex::new(vec![vec![1, 2], vec![3], vec![4, 5]]),
            deleted: Mutex::new(Vec::new()),
            searches: Mutex::new(0),
        };
        delete_doc_vectors(&backend, "doc-a", Some(&[0.1, 0.2, 0.3])).unwrap();
        assert_eq!(
            *backend.deleted.lock().unwrap(),
            vec![1, 2, 3, 4, 5],
            "kazdy przebieg musi skasowac to, co znalazl"
        );
        assert_eq!(
            *backend.searches.lock().unwrap(),
            4,
            "trzy przebiegi z trafieniami + jeden pusty konczacy petle"
        );
    }

    /// Petla ma twardy limit — backend, ktory zawsze cos zwraca, nie moze
    /// zawiesic ingestu.
    #[test]
    fn cleanup_is_bounded_by_max_passes() {
        let backend = PartialRecallBackend {
            pages: Mutex::new((0..100).map(|i| vec![i as u64 + 1]).collect()),
            deleted: Mutex::new(Vec::new()),
            searches: Mutex::new(0),
        };
        delete_doc_vectors(&backend, "doc-a", None).unwrap();
        assert_eq!(*backend.searches.lock().unwrap(), CLEANUP_MAX_PASSES);
    }

    #[test]
    fn ref_id_is_stable_and_never_zero() {
        assert_eq!(ref_id_for("doc-a", 0), ref_id_for("doc-a", 0));
        assert_ne!(ref_id_for("doc-a", 0), ref_id_for("doc-a", 1));
        assert_ne!(ref_id_for("doc-a", 0), ref_id_for("doc-b", 0));
        for i in 0..2000u64 {
            assert_ne!(ref_id_for("doc", i), 0, "ref_id 0 jest zarezerwowane");
        }
    }
}
