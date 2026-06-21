// ===== Plik: services/graph/error.rs — błędy warstwy grafowej (CozoDB) =====
//
// Jeden enum błędu współdzielony przez trait `GraphBackend`, implementację
// `CozoBackend` i `GraphManager`. Lustro `vector::error::VectorError` — warianty
// mapują się 1:1 na te same klasy problemów (nie znaleziono / już istnieje /
// quota / nazwa / I/O / błąd backendu/DB), plus warianty specyficzne dla grafu
// (`Datalog` — błąd wewnętrznego, host-budowanego zapytania Cozo; `ComputeBusy`
// — fail-closed z capa współbieżności ciężkich prymitywów). Host-fn dispatcher
// mapuje to na `AbiError` tak jak robi to vector.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GraphError {
    #[error("graph collection not found: org_id={org_id} addon_id={addon_id} collection={collection}")]
    CollectionNotFound {
        org_id: String,
        addon_id: String,
        collection: String,
    },

    #[error("graph collection already exists: org_id={org_id} addon_id={addon_id} collection={collection}")]
    CollectionExists {
        org_id: String,
        addon_id: String,
        collection: String,
    },

    #[error("quota exceeded: addon {addon_id} already has {current} graph collections (max {max})")]
    CollectionQuotaExceeded {
        addon_id: String,
        current: u32,
        max: u32,
    },

    #[error("quota exceeded: addon {addon_id} reached {current} graph nodes total (max {max})")]
    NodeQuotaExceeded {
        addon_id: String,
        current: u64,
        max: u64,
    },

    #[error("quota exceeded: addon {addon_id} reached {current} graph edges total (max {max})")]
    EdgeQuotaExceeded {
        addon_id: String,
        current: u64,
        max: u64,
    },

    #[error("invalid graph collection name '{0}' (must match ^[a-z0-9_-]{{1,64}}$)")]
    InvalidCollectionName(String),

    #[error("datalog error: {0}")]
    Datalog(String),

    #[error("graph compute capacity exhausted ({scope} limit {max}) — try again")]
    ComputeBusy { scope: &'static str, max: usize },

    #[error("graph backend error: {0}")]
    Backend(String),

    #[error("database error: {0}")]
    Db(String),

    #[error("io error at {path:?}: {source}")]
    Io {
        path: Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, GraphError>;
