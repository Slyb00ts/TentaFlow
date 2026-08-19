// ============ File: services/vector/namespace.rs — per-(org, addon) namespace registry (F1c P3 + F2 P1.c) ============
//
// Owns a process-wide `(org_id, addon_id, namespace) -> Arc<UsearchBackend>`
// cache keyed by `dashmap` for lock-free reads on hot lookup paths. Each open
// namespace corresponds to a row in `addon_vector_namespaces` (DB v27,
// org_id column added in v32) and a `.usearch` file under
// `<HOME>/.tentaflow/orgs/<org_id>/addons/<addon_id>/vectors/`.
//
// Quotas (F1c hard-coded, F2 makes them configurable):
//   * 10 namespaces per (org, addon)
//   * 1_000_000 vectors total per (org, addon) (summed across all namespaces)
//
// Cross-tenant isolation: lookup is always by (org_id, addon_id, namespace).
// There is no API surface that lets addon A in org X reach the same addon
// id's namespace in org Y; the org_id filters land on every SELECT / INSERT
// / UPDATE / DELETE and on every file-path resolution.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;

use super::backend::{Field, FieldSpec, Metric, SparseVector, UpsertItem, VectorBackend};
use super::error::{Result, VectorError};
use super::zvec_backend::ZvecBackend;
use crate::db::DbPool;
use tentaflow_sdk_spec::FieldType;

/// Stable string form of a `FieldType` for the `fields_json` DB column.
fn field_type_str(t: FieldType) -> &'static str {
    match t {
        FieldType::Str => "str",
        FieldType::Int => "int",
        FieldType::Float => "float",
        FieldType::Bool => "bool",
    }
}

fn field_type_from_str(s: &str) -> Option<FieldType> {
    match s {
        "str" => Some(FieldType::Str),
        "int" => Some(FieldType::Int),
        "float" => Some(FieldType::Float),
        "bool" => Some(FieldType::Bool),
        _ => None,
    }
}

/// Serialize a declared field schema to the JSON stored in
/// `addon_vector_namespaces.fields_json` (a `[{name,type,indexed}]` array). The
/// universal `FieldSpec` (minicbor, no serde) is mapped to a small serde mirror.
fn serialize_field_specs(fields: &[FieldSpec]) -> String {
    #[derive(serde::Serialize)]
    struct Stored<'a> {
        name: &'a str,
        #[serde(rename = "type")]
        ty: &'a str,
        indexed: bool,
    }
    let stored: Vec<Stored> = fields
        .iter()
        .map(|f| Stored {
            name: &f.name,
            ty: field_type_str(f.field_type),
            indexed: f.indexed,
        })
        .collect();
    serde_json::to_string(&stored).unwrap_or_else(|_| "[]".to_string())
}

/// Inverse of [`serialize_field_specs`]. An unknown type string is dropped (the
/// schema is reconstructed best-effort; the backend column would be missing,
/// surfacing as a clear filter/insert error rather than a silent wrong type).
fn parse_field_specs(json: &str) -> Vec<FieldSpec> {
    #[derive(serde::Deserialize)]
    struct Stored {
        name: String,
        #[serde(rename = "type")]
        ty: String,
        #[serde(default)]
        indexed: bool,
    }
    let stored: Vec<Stored> = serde_json::from_str(json).unwrap_or_default();
    stored
        .into_iter()
        .filter_map(|s| {
            field_type_from_str(&s.ty).map(|field_type| FieldSpec {
                name: s.name,
                field_type,
                indexed: s.indexed,
            })
        })
        .collect()
}

/// Hard cap on namespaces per (org, addon). Each open namespace holds a
/// usearch handle (mmap + connectivity graph), so we keep this modest in F1c.
pub const MAX_NAMESPACES_PER_ADDON: u32 = 10;

/// Hard cap on total vectors per (org, addon) (summed across all namespaces).
/// HNSW memory scales ~linearly with vector count; at 1 M × 512 dim × f32
/// the raw vector tape alone is ~2 GiB which is the budget ceiling we are
/// willing to hand a single addon install in F1c.
pub const MAX_VECTORS_PER_ADDON: u64 = 1_000_000;

/// Validates a namespace name. Names appear in file paths (so we must reject
/// `..`, `/`, control chars) and in DB primary keys; the allowed shape is
/// `[a-z0-9_-]{1,64}` which keeps the same charset as alias / camera ids.
pub fn validate_namespace_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(VectorError::InvalidNamespaceName(name.to_string()));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(VectorError::InvalidNamespaceName(name.to_string()));
    }
    Ok(())
}

/// Validates an addon id used as a path component. Same charset as namespace,
/// but allows uppercase to match the existing `addon_id` style used elsewhere
/// (e.g. `Tentaflow.Vision.Adr`). 128-char cap.
pub fn validate_addon_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 128 {
        return Err(VectorError::InvalidNamespaceName(id.to_string()));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
    {
        return Err(VectorError::InvalidNamespaceName(id.to_string()));
    }
    Ok(())
}

/// Validates the org id used as a path component. Org ids are UUIDv4 in
/// production but the default seed (`org-default`) and tempdir-based tests
/// use hyphenated lowercase strings; accept both as long as the charset
/// stays path-safe.
pub fn validate_org_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 64 {
        return Err(VectorError::InvalidNamespaceName(id.to_string()));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(VectorError::InvalidNamespaceName(id.to_string()));
    }
    Ok(())
}

/// Returns `<orgs_dir>/<org_id>/addons/<addon_id>/vectors/<namespace>.usearch`.
/// The org segment ensures the same addon installed in two tenants writes to
/// physically separate directories. Root idzie przez `paths::orgs_dir()`
/// (respektuje `addons_data_dir` z Ustawien).
fn namespace_file_path(org_id: &str, addon_id: &str, namespace: &str) -> Result<PathBuf> {
    Ok(crate::paths::orgs_dir()
        .join(org_id)
        .join("addons")
        .join(addon_id)
        .join("vectors")
        .join(format!("{namespace}.usearch")))
}

/// Validates a caller-supplied directory for creating a data collection (vector:
/// `get_or_create_at` / `*_with_quota`; graph:
/// `GraphManager::ensure_collection_at`). It must be absolute and free of `..` —
/// the path is later joined with a file name and stored in
/// `addon_vector_namespaces` / `addon_graph_collections` as the durable source of
/// truth about where the data lives, so a traversal would persist data outside
/// the data area for good.
///
/// Wartosc jest emitowana przez serwer (rejestr projektow), nie przez wolajacego
/// addona — to zabezpieczenie w glab, nie jedyna bariera.
pub(crate) fn validate_custom_dir(dir: &Path) -> Result<()> {
    if !dir.is_absolute() {
        return Err(VectorError::Db(format!(
            "collection data dir must be absolute: {}",
            dir.display()
        )));
    }
    if dir
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(VectorError::Db(format!(
            "collection data dir must not contain '..': {}",
            dir.display()
        )));
    }
    Ok(())
}

/// Reserved per-addon config keys (set by an admin via the vector backend picker,
/// keyed by addon_id == instance). Config is one structured JSON value; secrets
/// (Milvus auth) stay as separate `is_secret` rows so redaction/export keep working.
const CFG_VECTOR_CONFIG: &str = "__vector_config";
const CFG_MILVUS_USER: &str = "__vector_milvus_user";
const CFG_MILVUS_PASSWORD: &str = "__vector_milvus_password";

/// Odwolanie do serwisu Milvus w mesh (node + service id). Endpoint rozwiazywany
/// przy budowie polaczenia (nie zapisujemy runtime'owego URI w configu).
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct VectorServiceRef {
    #[serde(default)]
    node_id: String,
    #[serde(default)]
    service_id: String,
}

/// Strukturalny config backendu wektorowego instancji (`__vector_config`).
#[derive(Debug, Clone, serde::Deserialize)]
struct VectorBackendConfig {
    /// "zvec" (embedded, domyslny) | "milvus".
    #[serde(default = "default_vector_backend")]
    backend: String,
    /// Dla milvus: "service_ref" (z mesh) | "manual" (zewnetrzny URL).
    #[serde(default)]
    milvus_source: Option<String>,
    #[serde(default)]
    service_ref: Option<VectorServiceRef>,
    #[serde(default)]
    manual_uri: Option<String>,
    /// Opcjonalny suffix nazwy kolekcji (walidowany; nie zastepuje izolacji).
    #[serde(default)]
    collection_override: Option<String>,
}

fn default_vector_backend() -> String {
    "zvec".to_string()
}

impl Default for VectorBackendConfig {
    fn default() -> Self {
        Self {
            backend: default_vector_backend(),
            milvus_source: None,
            service_ref: None,
            manual_uri: None,
            collection_override: None,
        }
    }
}

/// 12 znakow hex z SHA-256 — deterministyczny, charset-safe segment nazwy.
fn hash12(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(s.as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Stabilna, charset-safe nazwa kolekcji Milvus. Milvus wymaga
/// `^[A-Za-z_][A-Za-z0-9_]{0,254}$`. Skladamy `v_o_<orghash>_a_<addonhash>_n_<nshash>`
/// (hash org/addon/namespace) — ten addon w dwoch tenantach NIGDY nie dzieli
/// kolekcji, a addon_id==instancja daje izolacje miedzy instancjami. (Crate
/// milvus-sdk-rust nie wspiera wyboru bazy, wiec izolacja jest na poziomie nazwy
/// kolekcji zamiast osobnej bazy per org.) `override` doklejany jako bezpieczny
/// suffix, nigdy nie zastepuje czesci izolacyjnej.
fn milvus_collection_name(
    org_id: &str,
    addon_id: &str,
    namespace: &str,
    override_suffix: Option<&str>,
) -> String {
    let mut s = format!(
        "v_o_{}_a_{}_n_{}",
        hash12(org_id),
        hash12(addon_id),
        hash12(namespace)
    );
    if let Some(o) = override_suffix.filter(|o| !o.is_empty()) {
        let safe: String = o
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(32)
            .collect();
        if !safe.is_empty() {
            s.push('_');
            s.push_str(&safe);
        }
    }
    s.truncate(255);
    s
}

/// `Some(service_ref)` when the config selects a REMOTE Milvus (source
/// `service_ref` with a non-empty `node_id`). Local refs and manual URLs → None,
/// so they fall through to the local (feature-gated) Milvus path.
fn remote_service_ref(cfg: &VectorBackendConfig) -> Option<&VectorServiceRef> {
    if cfg.milvus_source.as_deref() != Some("service_ref") {
        return None;
    }
    cfg.service_ref.as_ref().filter(|sr| !sr.node_id.is_empty())
}

/// Result of [`NamespaceManager::reconcile_namespace`] — the metadata columns
/// added and dropped to bring the live collection in line with the manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub added: Vec<String>,
    pub dropped: Vec<String>,
}

impl ReconcileReport {
    /// True when nothing changed (schema already matched).
    pub fn is_noop(&self) -> bool {
        self.added.is_empty() && self.dropped.is_empty()
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct NamespaceKey {
    org_id: String,
    addon_id: String,
    namespace: String,
}

pub struct NamespaceManager {
    pool: DbPool,
    backends: DashMap<NamespaceKey, Arc<dyn VectorBackend>>,
    /// Override for the on-disk root — production uses `dirs::home_dir()`;
    /// tests inject a tempdir to avoid polluting `~`.
    root_override: Option<PathBuf>,
    /// Mesh transport for remote (`service_ref` with a non-empty `node_id`)
    /// Milvus backends. Injected at router init via [`set_remote_transport`];
    /// building a remote backend before it is set fails loudly (local backends
    /// are unaffected). Swappable (RwLock, not OnceLock) so a mesh re-init
    /// rewires the transport instead of stranding backends on a stale manager.
    remote_transport: parking_lot::RwLock<Option<Arc<dyn super::remote::RemoteVectorTransport>>>,
}

impl NamespaceManager {
    pub fn new(pool: DbPool) -> Self {
        Self {
            pool,
            backends: DashMap::new(),
            root_override: None,
            remote_transport: parking_lot::RwLock::new(None),
        }
    }

    /// Constructor that pins the on-disk root under `root` instead of
    /// `~/.tentaflow`. Used by integration tests + future CLI workflows
    /// that need to operate on a sandboxed vectors tree.
    pub fn with_root(pool: DbPool, root: PathBuf) -> Self {
        Self {
            pool,
            backends: DashMap::new(),
            root_override: Some(root),
            remote_transport: parking_lot::RwLock::new(None),
        }
    }

    /// Inject (or replace) the mesh transport used to reach remote nodes' Milvus
    /// services. Called at router init; a later mesh re-init overwrites it so
    /// remote backends never strand on a stale manager.
    pub fn set_remote_transport(&self, transport: Arc<dyn super::remote::RemoteVectorTransport>) {
        *self.remote_transport.write() = Some(transport);
    }

    /// True once the mesh transport is wired — i.e. remote (`node_id`) backends
    /// can be built. The picker uses this to refuse a remote `service_ref` while
    /// the mesh is not yet up, instead of saving a config that fails on use.
    pub fn remote_transport_ready(&self) -> bool {
        self.remote_transport.read().is_some()
    }

    /// Drop every cached open backend for `addon_id`. Called after the vector
    /// backend config changes so the next access rebuilds against the new config
    /// (zvec ⇄ local Milvus ⇄ remote Milvus) without a process restart. The
    /// on-disk zvec data and remote collections are untouched — only the
    /// in-memory handle cache is cleared.
    pub fn invalidate_addon(&self, addon_id: &str) {
        self.backends.retain(|k, _| k.addon_id != addon_id);
    }

    /// Zrzuca WSZYSTKIE otwarte backendy (migracja katalogu danych addonow —
    /// uchwyty plikowe musza byc zamkniete przed przeniesieniem katalogu).
    /// Nastepny dostep otwiera indeks z nowej lokalizacji zapisanej w bazie.
    pub fn invalidate_all(&self) {
        self.backends.clear();
    }

    fn file_path_for(&self, org_id: &str, addon_id: &str, namespace: &str) -> Result<PathBuf> {
        if let Some(root) = &self.root_override {
            Ok(root
                .join(org_id)
                .join(addon_id)
                .join("vectors")
                .join(format!("{namespace}.usearch")))
        } else {
            namespace_file_path(org_id, addon_id, namespace)
        }
    }

    /// Read one reserved per-addon config value from `addon_config`. Returns
    /// `None` if absent or empty.
    fn addon_cfg(&self, addon_id: &str, key: &str) -> Option<String> {
        let conn = self.pool.read().ok()?;
        conn.query_row(
            "SELECT value FROM addon_config WHERE addon_id = ?1 AND key = ?2",
            rusqlite::params![addon_id, key],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .filter(|s| !s.is_empty())
    }

    /// Strukturalny config backendu wektorowego instancji. BRAK configu =>
    /// domyslny (zvec). Obecny ale NIEPOPRAWNY JSON => blad (nie cichy fallback
    /// na zvec — inaczej literowka w configu Milvus tworzylaby pusty namespace
    /// zvec i dane wygladalyby na utracone).
    fn vector_config(&self, addon_id: &str) -> Result<VectorBackendConfig> {
        // Czytamy raw (BEZ filtra pustych z addon_cfg), zeby odroznic brak wiersza
        // (=> default zvec) od obecnej, niepoprawnej wartosci (=> blad). Puste/
        // whitespace traktujemy jak brak (zvec).
        let raw: Option<String> = {
            let conn = match self.pool.read() {
                Ok(c) => c,
                Err(_) => return Ok(VectorBackendConfig::default()),
            };
            conn.query_row(
                "SELECT value FROM addon_config WHERE addon_id = ?1 AND key = ?2",
                rusqlite::params![addon_id, CFG_VECTOR_CONFIG],
                |r| r.get::<_, String>(0),
            )
            .ok()
        };
        match raw {
            None => Ok(VectorBackendConfig::default()),
            Some(s) if s.trim().is_empty() => Ok(VectorBackendConfig::default()),
            Some(s) => serde_json::from_str(&s).map_err(|e| {
                VectorError::Backend(format!(
                    "addon {addon_id}: niepoprawny __vector_config: {e}"
                ))
            }),
        }
    }

    /// Build the backend an addon's namespace should use. zvec (embedded, files
    /// at `file_path`) is the default; an admin can switch a specific addon to a
    /// local or cross-node Milvus via the reserved `__vector_config` config key.
    fn build_backend(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        dim: u32,
        metric: Metric,
        file_path: PathBuf,
        fields: &[FieldSpec],
        sparse: bool,
    ) -> Result<Arc<dyn VectorBackend>> {
        let cfg = self.vector_config(addon_id)?;
        match cfg.backend.as_str() {
            "zvec" | "" => Ok(Arc::new(ZvecBackend::open_or_create(
                file_path, dim, metric, fields, sparse,
            )?)),
            "milvus" => {
                // service_ref z niepustym node_id => Milvus na innym nodzie:
                // proxujemy operacje przez mesh (ten node NIE potrzebuje
                // feature vector-milvus, laczy sie tylko wlasciciel). W p.p.
                // lokalny Milvus (gated feature).
                match remote_service_ref(&cfg) {
                    Some(sr) => self.build_remote_milvus(
                        org_id, addon_id, namespace, dim, metric, fields, sparse, &cfg, sr,
                    ),
                    None => self.build_milvus(
                        org_id, addon_id, namespace, dim, metric, fields, sparse, &cfg,
                    ),
                }
            }
            // Nieznany backend => blad, nie cichy fallback na zvec (ochrona przed
            // utworzeniem pustego namespace zvec gdy intencja byla inna).
            other => Err(VectorError::Backend(format!(
                "addon {addon_id}: nieznany vector backend '{other}' (dozwolone: zvec|milvus)"
            ))),
        }
    }

    /// Build a `RemoteMeshVectorBackend` proxying ops to the Milvus owned by
    /// `sr.node_id`. Ungated on purpose: the client only ships CBOR over the
    /// mesh and never links the Milvus SDK — so a node without `vector-milvus`
    /// can still use another node's Milvus. The owner connects to its loopback.
    #[allow(clippy::too_many_arguments)]
    fn build_remote_milvus(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        dim: u32,
        metric: Metric,
        fields: &[FieldSpec],
        sparse: bool,
        cfg: &VectorBackendConfig,
        sr: &VectorServiceRef,
    ) -> Result<Arc<dyn VectorBackend>> {
        let transport = self.remote_transport.read().clone().ok_or_else(|| {
            VectorError::Backend(format!(
                "addon {addon_id}: zdalny Milvus (node {}) wymaga mesh, ale transport \
                 nie jest zainicjalizowany",
                sr.node_id
            ))
        })?;
        let collection = milvus_collection_name(
            org_id,
            addon_id,
            namespace,
            cfg.collection_override.as_deref(),
        );
        let user = self.addon_cfg(addon_id, CFG_MILVUS_USER);
        let password = self.addon_cfg(addon_id, CFG_MILVUS_PASSWORD);
        let be = super::remote::RemoteMeshVectorBackend::new(
            transport.clone(),
            sr.node_id.clone(),
            sr.service_id.clone(),
            collection,
            dim,
            metric,
            fields.to_vec(),
            sparse,
            user,
            password,
        );
        Ok(Arc::new(be))
    }

    #[cfg(feature = "vector-milvus")]
    fn build_milvus(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        dim: u32,
        metric: Metric,
        fields: &[FieldSpec],
        sparse: bool,
        cfg: &VectorBackendConfig,
    ) -> Result<Arc<dyn VectorBackend>> {
        let uri = self.resolve_milvus_uri(addon_id, cfg)?;
        let user = self.addon_cfg(addon_id, CFG_MILVUS_USER);
        let password = self.addon_cfg(addon_id, CFG_MILVUS_PASSWORD);
        let collection = milvus_collection_name(
            org_id,
            addon_id,
            namespace,
            cfg.collection_override.as_deref(),
        );
        let be = super::milvus_backend::MilvusBackend::connect(
            &uri,
            user.as_deref(),
            password.as_deref(),
            &collection,
            dim,
            metric,
            fields,
            sparse,
        )?;
        Ok(Arc::new(be))
    }

    /// Rozwiazuje URI Milvus z configu: manual (zewnetrzny URL) albo service_ref.
    /// Slice-1 rozwiazuje serwis LOKALNY (po service_id w `services`); zdalny
    /// (mesh, advertised_endpoint) dochodzi w nastepnym slice.
    #[cfg(feature = "vector-milvus")]
    fn resolve_milvus_uri(&self, addon_id: &str, cfg: &VectorBackendConfig) -> Result<String> {
        match cfg.milvus_source.as_deref() {
            Some("manual") => cfg
                .manual_uri
                .clone()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    VectorError::Backend(format!(
                        "addon {addon_id}: milvus_source=manual ale manual_uri jest puste"
                    ))
                }),
            Some("service_ref") => {
                let sr = cfg.service_ref.as_ref().ok_or_else(|| {
                    VectorError::Backend(format!(
                        "addon {addon_id}: milvus_source=service_ref ale service_ref brakuje"
                    ))
                })?;
                self.resolve_local_milvus_endpoint(sr).ok_or_else(|| {
                    VectorError::Backend(format!(
                        "addon {addon_id}: serwis Milvus '{}' nie znaleziony / nieosiagalny na tym nodzie",
                        sr.service_id
                    ))
                })
            }
            _ => Err(VectorError::Backend(format!(
                "addon {addon_id}: backend=milvus ale milvus_source nie ustawiony (service_ref|manual)"
            ))),
        }
    }

    /// Lokalny serwis Milvus -> endpoint_url. Slice-1 obsluguje TYLKO lokalny
    /// node: puste `node_id` == ten node. Niepuste `node_id` => ref zdalny, brak
    /// resolucji tu (mesh discovery w nastepnym slice) — to chroni przed
    /// trafieniem zdalnego `service_id` w lokalny wiersz o tym samym i64 id.
    /// Filtr: engine 'milvus', niespauzowany, status running/degraded, endpoint set.
    #[cfg(feature = "vector-milvus")]
    fn resolve_local_milvus_endpoint(&self, sr: &VectorServiceRef) -> Option<String> {
        use crate::services_repo::services::ServiceStatus;
        if !sr.node_id.is_empty() {
            return None;
        }
        let id: i64 = sr.service_id.parse().ok()?;
        let conn = self.pool.read().ok()?;
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

    #[cfg(not(feature = "vector-milvus"))]
    fn build_milvus(
        &self,
        _org_id: &str,
        _addon_id: &str,
        _namespace: &str,
        _dim: u32,
        _metric: Metric,
        _fields: &[FieldSpec],
        _sparse: bool,
        _cfg: &VectorBackendConfig,
    ) -> Result<Arc<dyn VectorBackend>> {
        Err(VectorError::Backend(
            "vector backend 'milvus' selected but this binary was built without the \
             'vector-milvus' feature"
                .to_string(),
        ))
    }

    /// Czy ten build ma wkompilowany backend Milvus (`vector-milvus`).
    pub fn milvus_compiled() -> bool {
        cfg!(feature = "vector-milvus")
    }

    /// Returns the namespace handle, opening (or creating) the backing index
    /// on first access. If a DB row for `(org_id, addon_id, namespace)`
    /// exists, its dim/metric must match the caller-supplied values —
    /// mismatch is an `Err` (an addon is not allowed to silently reshape an
    /// existing index).
    pub fn get_or_create(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        dim: u32,
        metric: Metric,
        fields: &[FieldSpec],
        sparse: bool,
    ) -> Result<Arc<dyn VectorBackend>> {
        self.get_or_create_inner(
            org_id, addon_id, namespace, dim, metric, fields, sparse, None,
        )
    }

    /// Like [`Self::get_or_create`], but a namespace created by this call lands
    /// at `custom_dir/<namespace>.usearch` instead of the addon vectors tree.
    /// Used for collections that belong to a non-addon owner (e.g. the Projects
    /// module: `data/projects/<project_id>/vectors/`). Validations and the
    /// per-(org, addon) quotas are identical to `get_or_create`.
    ///
    /// If the namespace already EXISTS (DB row present), `custom_dir` is
    /// ignored and the persisted `file_path` wins — the row is the single
    /// source of truth for where the data lives (`load_row` opens by it), so
    /// honoring a different directory on reopen would fork the collection and
    /// make the existing vectors look lost. Same-arguments reopen therefore
    /// behaves exactly like `get_or_create`.
    #[allow(clippy::too_many_arguments)]
    pub fn get_or_create_at(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        dim: u32,
        metric: Metric,
        fields: &[FieldSpec],
        sparse: bool,
        custom_dir: &Path,
    ) -> Result<Arc<dyn VectorBackend>> {
        self.get_or_create_inner(
            org_id,
            addon_id,
            namespace,
            dim,
            metric,
            fields,
            sparse,
            Some(custom_dir),
        )
    }

    /// Shared core of [`Self::get_or_create`] / [`Self::get_or_create_at`].
    /// `create_dir` only decides where a NOT-yet-existing namespace is created;
    /// every other code path (cache hit, existing row, quota, backend build)
    /// is common.
    #[allow(clippy::too_many_arguments)]
    fn get_or_create_inner(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        dim: u32,
        metric: Metric,
        fields: &[FieldSpec],
        sparse: bool,
        create_dir: Option<&Path>,
    ) -> Result<Arc<dyn VectorBackend>> {
        validate_org_id(org_id)?;
        validate_addon_id(addon_id)?;
        validate_namespace_name(namespace)?;
        if !(1..=4096).contains(&dim) {
            return Err(VectorError::InvalidDim(dim));
        }

        let key = NamespaceKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            namespace: namespace.to_string(),
        };

        if let Some(be) = self.backends.get(&key) {
            let be = be.clone();
            if be.dim() != dim {
                return Err(VectorError::DimMismatch {
                    expected: be.dim(),
                    actual: dim,
                });
            }
            if be.metric() != metric {
                return Err(VectorError::MetricMismatch {
                    expected: be.metric().as_str(),
                    actual: metric.as_str().to_string(),
                });
            }
            return Ok(be);
        }

        let existing = self.load_row(org_id, addon_id, namespace)?;
        let (resolved_dim, resolved_metric, file_path, resolved_fields, resolved_sparse) =
            match existing {
                Some((
                    existing_dim,
                    existing_metric,
                    existing_path,
                    existing_fields,
                    existing_sparse,
                )) => {
                    if existing_dim != dim {
                        return Err(VectorError::DimMismatch {
                            expected: existing_dim,
                            actual: dim,
                        });
                    }
                    if existing_metric != metric {
                        return Err(VectorError::MetricMismatch {
                            expected: existing_metric.as_str(),
                            actual: metric.as_str().to_string(),
                        });
                    }
                    // The stored schema is authoritative; declaring a different field
                    // set on reopen does not silently reshape the collection.
                    // Reconciliation (add/drop column on addon update) is a separate,
                    // explicit operation.
                    (
                        existing_dim,
                        existing_metric,
                        existing_path,
                        existing_fields,
                        existing_sparse,
                    )
                }
                None => {
                    self.check_namespace_quota(org_id, addon_id)?;
                    let path = match create_dir {
                        Some(dir) => {
                            validate_custom_dir(dir)?;
                            dir.join(format!("{namespace}.usearch"))
                        }
                        None => self.file_path_for(org_id, addon_id, namespace)?,
                    };
                    self.insert_row(
                        org_id, addon_id, namespace, dim, metric, &path, fields, sparse,
                    )?;
                    (dim, metric, path, fields.to_vec(), sparse)
                }
            };

        let backend = self.build_backend(
            org_id,
            addon_id,
            namespace,
            resolved_dim,
            resolved_metric,
            file_path,
            &resolved_fields,
            resolved_sparse,
        )?;

        let entry = self.backends.entry(key).or_insert(backend);
        Ok(entry.value().clone())
    }

    /// Lookup without creation — used by `vector_search_v1` / `vector_delete_v1`.
    pub fn get(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
    ) -> Result<Arc<dyn VectorBackend>> {
        validate_org_id(org_id)?;
        validate_addon_id(addon_id)?;
        validate_namespace_name(namespace)?;
        let key = NamespaceKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            namespace: namespace.to_string(),
        };
        if let Some(be) = self.backends.get(&key) {
            return Ok(be.clone());
        }
        let row = self.load_row(org_id, addon_id, namespace)?;
        let Some((dim, metric, file_path, fields, sparse)) = row else {
            return Err(VectorError::NamespaceNotFound {
                addon_id: addon_id.to_string(),
                namespace: namespace.to_string(),
            });
        };
        let backend = self.build_backend(
            org_id, addon_id, namespace, dim, metric, file_path, &fields, sparse,
        )?;
        let entry = self.backends.entry(key).or_insert(backend);
        Ok(entry.value().clone())
    }

    /// Transactional upsert scoped to `(org_id, addon_id)`. Checks the
    /// per-tenant quota, runs the backend upsert (which persists internally),
    /// and bumps the cached `count` row — all under a single `IMMEDIATE`
    /// SQLite transaction so two concurrent upserts at the cap cannot both
    /// succeed.
    ///
    /// Returns the new count for the namespace.
    ///
    /// `create_dir` dziala jak w [`Self::get_or_create_at`]: wskazuje katalog dla
    /// przestrzeni, ktora jeszcze NIE istnieje (wlasciciel spoza drzewa addonow,
    /// np. projekt). Dla istniejacego wiersza jest ignorowany — zrodlem prawdy o
    /// lokalizacji zostaje zapisany `file_path`.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_with_quota(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        ref_id: u64,
        vector: &[f32],
        dim: u32,
        metric: Metric,
        field_specs: &[FieldSpec],
        field_values: &[Field],
        sparse_flag: bool,
        sparse_value: Option<&SparseVector>,
        create_dir: Option<&Path>,
    ) -> Result<u64> {
        let backend = self.get_or_create_inner(
            org_id,
            addon_id,
            namespace,
            dim,
            metric,
            field_specs,
            sparse_flag,
            create_dir,
        )?;
        let is_replace = backend.has_ref(ref_id);

        let conn = self
            .pool
            .write()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| VectorError::Db(e.to_string()))?;

        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(count), 0) FROM addon_vector_namespaces \
                 WHERE addon_id = ?1 AND org_id = ?2",
                rusqlite::params![addon_id, org_id],
                |r| r.get(0),
            )
            .map_err(|e| {
                let _ = conn.execute("ROLLBACK", []);
                VectorError::Db(e.to_string())
            })?;

        if !is_replace && (total as u64) >= MAX_VECTORS_PER_ADDON {
            let _ = conn.execute("ROLLBACK", []);
            return Err(VectorError::VectorQuotaExceeded {
                addon_id: addon_id.to_string(),
                current: total as u64,
                max: MAX_VECTORS_PER_ADDON,
            });
        }

        if let Err(e) = backend.upsert(ref_id, vector, field_values, sparse_value) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(e);
        }

        let new_count = backend.count();
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        if let Err(e) = conn.execute(
            "UPDATE addon_vector_namespaces SET count = ?1, updated_at = ?2 \
             WHERE addon_id = ?3 AND namespace = ?4 AND org_id = ?5",
            rusqlite::params![new_count as i64, now, addon_id, namespace, org_id],
        ) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(VectorError::Db(e.to_string()));
        }
        conn.execute("COMMIT", [])
            .map_err(|e| VectorError::Db(e.to_string()))?;
        Ok(new_count)
    }

    /// Transactional BATCH upsert scoped to `(org_id, addon_id, namespace)`.
    /// Counts only realistically-new `ref_id`s (those not already stored) toward
    /// the per-tenant quota, in a SINGLE `IMMEDIATE` transaction, then runs ONE
    /// batched backend insert (zvec builds its HNSW graph from all docs at once).
    /// A backend failure rolls the transaction back so the cached count and the
    /// index stay consistent. Returns the new namespace count.
    ///
    /// All items target the same namespace/dim/metric/schema; per-item `ref_id`,
    /// `vector`, `fields` and `sparse` come from `items`.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_batch_with_quota(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        dim: u32,
        metric: Metric,
        field_specs: &[FieldSpec],
        sparse_flag: bool,
        items: &[UpsertItem<'_>],
        create_dir: Option<&Path>,
    ) -> Result<u64> {
        if items.is_empty() {
            return Ok(self
                .get_or_create_inner(
                    org_id,
                    addon_id,
                    namespace,
                    dim,
                    metric,
                    field_specs,
                    sparse_flag,
                    create_dir,
                )?
                .count());
        }

        let backend = self.get_or_create_inner(
            org_id,
            addon_id,
            namespace,
            dim,
            metric,
            field_specs,
            sparse_flag,
            create_dir,
        )?;

        // Count genuinely new ref_ids (replaces consume no quota). A duplicate
        // ref_id within the same batch counts once.
        let mut new_refs: HashSet<u64> = HashSet::with_capacity(items.len());
        for item in items {
            if !backend.has_ref(item.ref_id) {
                new_refs.insert(item.ref_id);
            }
        }
        let new_inserts = new_refs.len() as u64;

        let conn = self
            .pool
            .write()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| VectorError::Db(e.to_string()))?;

        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(count), 0) FROM addon_vector_namespaces \
                 WHERE addon_id = ?1 AND org_id = ?2",
                rusqlite::params![addon_id, org_id],
                |r| r.get(0),
            )
            .map_err(|e| {
                let _ = conn.execute("ROLLBACK", []);
                VectorError::Db(e.to_string())
            })?;

        if total as u64 + new_inserts > MAX_VECTORS_PER_ADDON {
            let _ = conn.execute("ROLLBACK", []);
            return Err(VectorError::VectorQuotaExceeded {
                addon_id: addon_id.to_string(),
                current: total as u64,
                max: MAX_VECTORS_PER_ADDON,
            });
        }

        if let Err(e) = backend.upsert_batch(items) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(e);
        }

        let new_count = backend.count();
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        if let Err(e) = conn.execute(
            "UPDATE addon_vector_namespaces SET count = ?1, updated_at = ?2 \
             WHERE addon_id = ?3 AND namespace = ?4 AND org_id = ?5",
            rusqlite::params![new_count as i64, now, addon_id, namespace, org_id],
        ) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(VectorError::Db(e.to_string()));
        }
        conn.execute("COMMIT", [])
            .map_err(|e| VectorError::Db(e.to_string()))?;
        Ok(new_count)
    }

    fn check_namespace_quota(&self, org_id: &str, addon_id: &str) -> Result<()> {
        let conn = self
            .pool
            .read()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM addon_vector_namespaces \
                 WHERE addon_id = ?1 AND org_id = ?2",
                rusqlite::params![addon_id, org_id],
                |r| r.get(0),
            )
            .map_err(|e| VectorError::Db(e.to_string()))?;
        if count as u32 >= MAX_NAMESPACES_PER_ADDON {
            return Err(VectorError::NamespaceQuotaExceeded {
                addon_id: addon_id.to_string(),
                current: count as u32,
                max: MAX_NAMESPACES_PER_ADDON,
            });
        }
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    fn load_row(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
    ) -> Result<Option<(u32, Metric, PathBuf, Vec<FieldSpec>, bool)>> {
        let conn = self
            .pool
            .read()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        let row = conn
            .query_row(
                "SELECT dim, metric, file_path, fields_json, sparse FROM addon_vector_namespaces \
                 WHERE addon_id = ?1 AND namespace = ?2 AND org_id = ?3",
                rusqlite::params![addon_id, namespace, org_id],
                |r| {
                    let dim: i64 = r.get(0)?;
                    let metric: String = r.get(1)?;
                    let path: String = r.get(2)?;
                    let fields_json: String = r.get(3)?;
                    let sparse: i64 = r.get(4)?;
                    Ok((
                        dim as u32,
                        metric,
                        PathBuf::from(path),
                        fields_json,
                        sparse != 0,
                    ))
                },
            )
            .ok();
        let Some((dim, metric_str, path, fields_json, sparse)) = row else {
            return Ok(None);
        };
        let metric = Metric::parse(&metric_str)
            .ok_or_else(|| VectorError::Db(format!("invalid metric '{metric_str}' in DB row")))?;
        Ok(Some((
            dim,
            metric,
            path,
            parse_field_specs(&fields_json),
            sparse,
        )))
    }

    fn insert_row(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        dim: u32,
        metric: Metric,
        file_path: &PathBuf,
        fields: &[FieldSpec],
        sparse: bool,
    ) -> Result<()> {
        let conn = self
            .pool
            .write()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        conn.execute(
            "INSERT INTO addon_vector_namespaces \
             (addon_id, namespace, dim, metric, count, file_path, created_at, updated_at, org_id, fields_json, sparse) \
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                addon_id,
                namespace,
                dim as i64,
                metric.as_str(),
                file_path.to_string_lossy().to_string(),
                now,
                org_id,
                serialize_field_specs(fields),
                sparse as i64,
            ],
        )
        .map_err(|e| VectorError::Db(e.to_string()))?;
        Ok(())
    }

    /// Refreshes the cached `count` column after an upsert/delete. Done as a
    /// separate UPDATE to keep the per-write critical path short (the heavy
    /// usearch save() already happened by the time we get here).
    pub fn update_count(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        new_count: u64,
    ) -> Result<()> {
        let conn = self
            .pool
            .write()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        conn.execute(
            "UPDATE addon_vector_namespaces SET count = ?1, updated_at = ?2 \
             WHERE addon_id = ?3 AND namespace = ?4 AND org_id = ?5",
            rusqlite::params![new_count as i64, now, addon_id, namespace, org_id],
        )
        .map_err(|e| VectorError::Db(e.to_string()))?;
        Ok(())
    }

    /// Reconcile the live schema of one namespace against the addon's currently
    /// declared `[[vector_namespace]].fields`. Called on addon update. Adds new
    /// columns, drops removed ones, and rebuilds a column whose type changed
    /// (drop + add — the stored values are incompatible with the new type). A
    /// namespace with no DB row yet is a no-op: it will be created with the new
    /// schema on first use. Returns the applied diff for auditing.
    pub fn reconcile_namespace(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        desired: &[FieldSpec],
    ) -> Result<ReconcileReport> {
        validate_org_id(org_id)?;
        validate_addon_id(addon_id)?;
        validate_namespace_name(namespace)?;

        let Some((_dim, _metric, _path, stored, _sparse)) =
            self.load_row(org_id, addon_id, namespace)?
        else {
            return Ok(ReconcileReport::default());
        };

        let stored_type = |name: &str| stored.iter().find(|f| f.name == name).map(|f| f.field_type);
        let desired_type = |name: &str| {
            desired
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.field_type)
        };

        let mut to_drop: Vec<String> = Vec::new();
        let mut to_add: Vec<FieldSpec> = Vec::new();

        for s in &stored {
            match desired_type(&s.name) {
                None => to_drop.push(s.name.clone()),
                Some(dt) if dt != s.field_type => to_drop.push(s.name.clone()),
                Some(_) => {}
            }
        }
        for d in desired {
            match stored_type(&d.name) {
                None => to_add.push(d.clone()),
                Some(st) if st != d.field_type => to_add.push(d.clone()),
                Some(_) => {}
            }
        }

        if to_drop.is_empty() && to_add.is_empty() {
            return Ok(ReconcileReport::default());
        }

        // The backend applies the change however its engine allows (zvec
        // rebuilds the collection; Milvus adds columns online and errors on a
        // removal). Only on success do we record the new schema, so a failed
        // reconcile leaves `fields_json` matching the live collection.
        let backend = self.get(org_id, addon_id, namespace)?;
        backend.reconcile_fields(&stored, desired)?;

        self.update_fields_json(org_id, addon_id, namespace, desired)?;
        Ok(ReconcileReport {
            added: to_add.into_iter().map(|f| f.name).collect(),
            dropped: to_drop,
        })
    }

    fn update_fields_json(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        fields: &[FieldSpec],
    ) -> Result<()> {
        let conn = self
            .pool
            .write()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        conn.execute(
            "UPDATE addon_vector_namespaces SET fields_json = ?1, updated_at = ?2 \
             WHERE addon_id = ?3 AND namespace = ?4 AND org_id = ?5",
            rusqlite::params![
                serialize_field_specs(fields),
                now,
                addon_id,
                namespace,
                org_id,
            ],
        )
        .map_err(|e| VectorError::Db(e.to_string()))?;
        Ok(())
    }

    /// Admin op — drops both the DB row and the on-disk file. Not exposed to
    /// addons (no host function); reached from the CLI in a later phase.
    /// Idempotent: missing row / missing file are both treated as success so
    /// the operation can be retried after a partial failure.
    pub fn delete_namespace(&self, org_id: &str, addon_id: &str, namespace: &str) -> Result<()> {
        validate_org_id(org_id)?;
        validate_addon_id(addon_id)?;
        validate_namespace_name(namespace)?;
        let key = NamespaceKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            namespace: namespace.to_string(),
        };
        self.backends.remove(&key);

        let path = {
            let conn = self
                .pool
                .write()
                .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
            let path: Option<String> = conn
                .query_row(
                    "SELECT file_path FROM addon_vector_namespaces \
                     WHERE addon_id = ?1 AND namespace = ?2 AND org_id = ?3",
                    rusqlite::params![addon_id, namespace, org_id],
                    |r| r.get(0),
                )
                .ok();
            conn.execute(
                "DELETE FROM addon_vector_namespaces \
                 WHERE addon_id = ?1 AND namespace = ?2 AND org_id = ?3",
                rusqlite::params![addon_id, namespace, org_id],
            )
            .map_err(|e| VectorError::Db(e.to_string()))?;
            path.map(PathBuf::from)
        };

        if let Some(p) = path {
            if p.exists() {
                // A zvec namespace is a collection *directory* on disk.
                let res = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
                res.map_err(|e| VectorError::Io {
                    path: Some(p),
                    source: e,
                })?;
            }
        }
        Ok(())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::Arc;
    use tempfile::TempDir;

    const ORG_A: &str = "org-a";
    const ORG_B: &str = "org-b";

    fn in_memory_db_with_v27() -> DbPool {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn mgr() -> (TempDir, NamespaceManager) {
        let dir = TempDir::new().unwrap();
        let pool = in_memory_db_with_v27();
        let mgr = NamespaceManager::with_root(pool, dir.path().to_path_buf());
        (dir, mgr)
    }

    #[test]
    fn test_get_or_create_first_call_creates_row() {
        let (_dir, mgr) = mgr();
        let be = mgr
            .get_or_create(ORG_A, "addon_a", "faces", 4, Metric::Cosine, &[], false)
            .unwrap();
        assert_eq!(be.count(), 0);
        be.upsert(1, &[1.0, 0.0, 0.0, 0.0], &[], None).unwrap();
        assert_eq!(be.count(), 1);
    }

    #[test]
    fn test_get_or_create_idempotent() {
        let (_dir, mgr) = mgr();
        let a = mgr
            .get_or_create(ORG_A, "addon_a", "faces", 4, Metric::Cosine, &[], false)
            .unwrap();
        let b = mgr
            .get_or_create(ORG_A, "addon_a", "faces", 4, Metric::Cosine, &[], false)
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn test_get_or_create_at_custom_dir_create_reopen_search() {
        let root = TempDir::new().unwrap();
        let custom = TempDir::new().unwrap();
        let pool = in_memory_db_with_v27();
        // Nested, not-yet-existing dir: the backend must create it on open.
        let custom_dir = custom.path().join("projects").join("p1").join("vectors");

        {
            let mgr = NamespaceManager::with_root(pool.clone(), root.path().to_path_buf());
            let be = mgr
                .get_or_create_at(
                    ORG_A,
                    "ps-proj1",
                    "docs",
                    3,
                    Metric::Cosine,
                    &[],
                    false,
                    &custom_dir,
                )
                .unwrap();
            be.upsert(1, &[1.0, 0.0, 0.0], &[], None).unwrap();
            be.upsert(2, &[0.0, 1.0, 0.0], &[], None).unwrap();
            be.save().unwrap();

            // The persisted file_path must live under custom_dir, NOT the
            // manager root — that is the whole point of the variant.
            let conn = pool.read().unwrap();
            let p: String = conn
                .query_row(
                    "SELECT file_path FROM addon_vector_namespaces \
                     WHERE addon_id='ps-proj1' AND namespace='docs' AND org_id='org-a'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let stored = PathBuf::from(&p);
            assert!(stored.starts_with(&custom_dir), "stored path: {p}");
            assert!(stored.exists());
        }

        // Fresh manager (empty backend cache): reopen resolves through the
        // persisted file_path, so the custom-dir data survives a restart.
        let mgr2 = NamespaceManager::with_root(pool, root.path().to_path_buf());
        let be = mgr2.get(ORG_A, "ps-proj1", "docs").unwrap();
        assert_eq!(be.count(), 2);
        let hits = be.search(&[1.0, 0.0, 0.0], 1, None, &[]).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].ref_id, 1);

        // Reopening via get_or_create_at with a DIFFERENT dir keeps the
        // persisted path (same handle, no fork).
        let other = TempDir::new().unwrap();
        let be2 = mgr2
            .get_or_create_at(
                ORG_A,
                "ps-proj1",
                "docs",
                3,
                Metric::Cosine,
                &[],
                false,
                other.path(),
            )
            .unwrap();
        assert!(Arc::ptr_eq(&be, &be2));
        assert_eq!(be2.count(), 2);
    }

    #[test]
    fn test_get_or_create_at_enforces_namespace_quota() {
        let (_dir, mgr) = mgr();
        let custom = TempDir::new().unwrap();
        for i in 0..MAX_NAMESPACES_PER_ADDON {
            mgr.get_or_create_at(
                ORG_A,
                "ps-proj1",
                &format!("ns{i}"),
                4,
                Metric::Cosine,
                &[],
                false,
                custom.path(),
            )
            .unwrap();
        }
        let res = mgr.get_or_create_at(
            ORG_A,
            "ps-proj1",
            "overflow",
            4,
            Metric::Cosine,
            &[],
            false,
            custom.path(),
        );
        assert!(matches!(
            res,
            Err(VectorError::NamespaceQuotaExceeded { .. })
        ));
    }

    #[test]
    fn test_dim_mismatch_on_reopen_rejected() {
        let (_dir, mgr) = mgr();
        mgr.get_or_create(ORG_A, "addon_a", "faces", 4, Metric::Cosine, &[], false)
            .unwrap();
        let res = mgr.get_or_create(ORG_A, "addon_a", "faces", 8, Metric::Cosine, &[], false);
        assert!(matches!(res, Err(VectorError::DimMismatch { .. })));
    }

    #[test]
    fn test_quota_exceeded_at_max_namespaces() {
        let (_dir, mgr) = mgr();
        for i in 0..MAX_NAMESPACES_PER_ADDON {
            mgr.get_or_create(
                ORG_A,
                "addon_a",
                &format!("ns{i}"),
                4,
                Metric::Cosine,
                &[],
                false,
            )
            .unwrap();
        }
        let res = mgr.get_or_create(ORG_A, "addon_a", "overflow", 4, Metric::Cosine, &[], false);
        assert!(matches!(
            res,
            Err(VectorError::NamespaceQuotaExceeded { .. })
        ));
    }

    #[test]
    fn test_delete_namespace_removes_file_and_db_row() {
        let (_dir, mgr) = mgr();
        let be = mgr
            .get_or_create(ORG_A, "addon_a", "faces", 3, Metric::Cosine, &[], false)
            .unwrap();
        be.upsert(1, &[1.0, 0.0, 0.0], &[], None).unwrap();
        be.save().unwrap();
        let file_path = {
            let conn = mgr.pool.read().unwrap();
            let p: String = conn
                .query_row(
                    "SELECT file_path FROM addon_vector_namespaces \
                     WHERE addon_id='addon_a' AND namespace='faces' AND org_id='org-a'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            PathBuf::from(p)
        };
        assert!(file_path.exists());

        mgr.delete_namespace(ORG_A, "addon_a", "faces").unwrap();
        assert!(!file_path.exists());
        let row: Option<i64> = {
            let conn = mgr.pool.read().unwrap();
            conn.query_row(
                "SELECT 1 FROM addon_vector_namespaces \
                 WHERE addon_id='addon_a' AND namespace='faces' AND org_id='org-a'",
                [],
                |r| r.get(0),
            )
            .ok()
        };
        assert!(row.is_none());
    }

    #[test]
    fn test_cross_addon_namespace_isolation() {
        let (_dir, mgr) = mgr();
        let a = mgr
            .get_or_create(ORG_A, "addon_a", "faces", 3, Metric::Cosine, &[], false)
            .unwrap();
        let b = mgr
            .get_or_create(ORG_A, "addon_b", "faces", 3, Metric::Cosine, &[], false)
            .unwrap();
        a.upsert(1, &[1.0, 0.0, 0.0], &[], None).unwrap();
        assert_eq!(a.count(), 1);
        assert_eq!(b.count(), 0);
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn test_cross_org_get_returns_not_found() {
        // F2 P1.c — same addon_id + namespace lookup under a different org
        // must NOT see the other tenant's namespace. The SQL WHERE clause is
        // `addon_id = ? AND namespace = ? AND org_id = ?`; the migration v32
        // PK `(addon_id, namespace)` still applies, so the production
        // assumption is that no two orgs share the same literal `addon_id`
        // (per-org install paths in P1.b enforce this — `addon_data_dir`
        // composes org_id into the path). The test below exercises the SQL
        // org_id filter directly: even with the row physically present in
        // org A, a lookup under org B sees nothing.
        let (_dir, mgr) = mgr();
        mgr.get_or_create(
            ORG_A,
            "addon_x_query",
            "faces",
            3,
            Metric::Cosine,
            &[],
            false,
        )
        .unwrap();
        let res = mgr.get(ORG_B, "addon_x_query", "faces");
        assert!(matches!(res, Err(VectorError::NamespaceNotFound { .. })));
    }

    #[test]
    fn test_get_missing_namespace_returns_not_found() {
        let (_dir, mgr) = mgr();
        let res = mgr.get(ORG_A, "addon_x", "missing");
        assert!(matches!(res, Err(VectorError::NamespaceNotFound { .. })));
    }

    #[test]
    fn test_invalid_namespace_name_rejected() {
        let (_dir, mgr) = mgr();
        let res = mgr.get_or_create(ORG_A, "addon_a", "bad/name", 3, Metric::Cosine, &[], false);
        assert!(matches!(res, Err(VectorError::InvalidNamespaceName(_))));
    }

    #[test]
    fn test_upsert_with_quota_replace_does_not_increment_count() {
        let (_dir, mgr) = mgr();
        let c1 = mgr
            .upsert_with_quota(
                ORG_A,
                "addon_a",
                "ns1",
                1,
                &[1.0, 0.0, 0.0],
                3,
                Metric::Cosine,
                &[],
                &[],
                false,
                None,
                None,
            )
            .unwrap();
        assert_eq!(c1, 1);
        let c2 = mgr
            .upsert_with_quota(
                ORG_A,
                "addon_a",
                "ns1",
                1,
                &[0.0, 1.0, 0.0],
                3,
                Metric::Cosine,
                &[],
                &[],
                false,
                None,
                None,
            )
            .unwrap();
        assert_eq!(c2, 1);
    }

    #[test]
    fn test_upsert_batch_with_quota_counts_only_new_refs() {
        let (_dir, mgr) = mgr();
        let v1 = [1.0, 0.0, 0.0];
        let v2 = [0.0, 1.0, 0.0];
        let v3 = [0.0, 0.0, 1.0];

        // Three distinct refs in one batch -> count 3.
        let items = vec![
            UpsertItem {
                ref_id: 1,
                vector: &v1,
                fields: &[],
                sparse: None,
            },
            UpsertItem {
                ref_id: 2,
                vector: &v2,
                fields: &[],
                sparse: None,
            },
            UpsertItem {
                ref_id: 3,
                vector: &v3,
                fields: &[],
                sparse: None,
            },
        ];
        let c1 = mgr
            .upsert_batch_with_quota(
                ORG_A,
                "addon_a",
                "ns1",
                3,
                Metric::Cosine,
                &[],
                false,
                &items,
                None,
            )
            .unwrap();
        assert_eq!(c1, 3);

        // Re-batch the same refs (all replaces) -> count stays 3 (no quota delta).
        let c2 = mgr
            .upsert_batch_with_quota(
                ORG_A,
                "addon_a",
                "ns1",
                3,
                Metric::Cosine,
                &[],
                false,
                &items,
                None,
            )
            .unwrap();
        assert_eq!(c2, 3);

        // One new ref + two replaces -> count 4.
        let v4 = [0.5, 0.5, 0.0];
        let mixed = vec![
            UpsertItem {
                ref_id: 1,
                vector: &v1,
                fields: &[],
                sparse: None,
            },
            UpsertItem {
                ref_id: 4,
                vector: &v4,
                fields: &[],
                sparse: None,
            },
        ];
        let c3 = mgr
            .upsert_batch_with_quota(
                ORG_A,
                "addon_a",
                "ns1",
                3,
                Metric::Cosine,
                &[],
                false,
                &mixed,
                None,
            )
            .unwrap();
        assert_eq!(c3, 4);
    }

    #[test]
    fn test_declared_fields_persist_and_filter_through_manager() {
        use tentaflow_sdk_spec::{FieldValue, Filter};
        let dir = TempDir::new().unwrap();
        let pool = in_memory_db_with_v27();
        let specs = vec![FieldSpec {
            name: "source".to_string(),
            field_type: FieldType::Str,
            indexed: true,
        }];
        let values_inbox = vec![Field {
            name: "source".to_string(),
            value: FieldValue::Str("inbox".to_string()),
        }];
        let values_web = vec![Field {
            name: "source".to_string(),
            value: FieldValue::Str("web".to_string()),
        }];

        {
            let mgr = NamespaceManager::with_root(pool.clone(), dir.path().to_path_buf());
            mgr.upsert_with_quota(
                ORG_A,
                "addon_meta",
                "docs",
                1,
                &[1.0, 0.0, 0.0],
                3,
                Metric::Cosine,
                &specs,
                &values_inbox,
                false,
                None,
                None,
            )
            .unwrap();
            mgr.upsert_with_quota(
                ORG_A,
                "addon_meta",
                "docs",
                2,
                &[0.0, 1.0, 0.0],
                3,
                Metric::Cosine,
                &specs,
                &values_web,
                false,
                None,
                None,
            )
            .unwrap();

            // The schema must round-trip through the DB column.
            let conn = pool.read().unwrap();
            let fields_json: String = conn
                .query_row(
                    "SELECT fields_json FROM addon_vector_namespaces \
                     WHERE addon_id='addon_meta' AND namespace='docs' AND org_id='org-a'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(fields_json.contains("\"source\""));
            assert!(fields_json.contains("\"str\""));
        }

        // Reopen with a fresh manager (empty backend cache): the schema is
        // reconstructed from `fields_json`, so a filtered search still works.
        let mgr2 = NamespaceManager::with_root(pool, dir.path().to_path_buf());
        let be = mgr2.get(ORG_A, "addon_meta", "docs").unwrap();
        let hits = be
            .search(
                &[1.0, 0.0, 0.0],
                10,
                Some(&Filter::Eq(
                    "source".to_string(),
                    FieldValue::Str("inbox".to_string()),
                )),
                &["source".to_string()],
            )
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "filter source='inbox' should match one vector"
        );
        assert_eq!(hits[0].ref_id, 1);
        assert_eq!(
            hits[0].fields.first().map(|f| &f.value),
            Some(&FieldValue::Str("inbox".to_string()))
        );
    }

    #[test]
    fn test_reconcile_namespace_adds_and_drops_fields() {
        use tentaflow_sdk_spec::{FieldValue, Filter};
        let (_dir, mgr) = mgr();

        // Create with one field "source" and a vector tagged with it.
        let initial = vec![FieldSpec {
            name: "source".to_string(),
            field_type: FieldType::Str,
            indexed: true,
        }];
        mgr.upsert_with_quota(
            ORG_A,
            "addon_r",
            "docs",
            1,
            &[1.0, 0.0, 0.0],
            3,
            Metric::Cosine,
            &initial,
            &[Field {
                name: "source".to_string(),
                value: FieldValue::Str("inbox".to_string()),
            }],
            false,
            None,
            None,
        )
        .unwrap();

        // New manifest: drop "source", add "score" (Int). Reconcile.
        let desired = vec![FieldSpec {
            name: "score".to_string(),
            field_type: FieldType::Int,
            indexed: true,
        }];
        let report = mgr
            .reconcile_namespace(ORG_A, "addon_r", "docs", &desired)
            .unwrap();
        assert_eq!(report.added, vec!["score".to_string()]);
        assert_eq!(report.dropped, vec!["source".to_string()]);

        // fields_json now reflects the new schema.
        {
            let conn = mgr.pool.read().unwrap();
            let json: String = conn
                .query_row(
                    "SELECT fields_json FROM addon_vector_namespaces \
                     WHERE addon_id='addon_r' AND namespace='docs' AND org_id='org-a'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(json.contains("\"score\""));
            assert!(!json.contains("\"source\""));
        }

        // The new column is now usable: upsert with "score" and filter on it.
        let be = mgr.get(ORG_A, "addon_r", "docs").unwrap();
        be.upsert(
            2,
            &[0.0, 1.0, 0.0],
            &[Field {
                name: "score".to_string(),
                value: FieldValue::Int(99),
            }],
            None,
        )
        .unwrap();
        let hits = be
            .search(
                &[0.0, 1.0, 0.0],
                5,
                Some(&Filter::Gt("score".to_string(), FieldValue::Int(50))),
                &["score".to_string()],
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].ref_id, 2);
    }

    #[test]
    fn test_hybrid_search_through_manager() {
        use tentaflow_sdk_spec::Fusion;
        let (_dir, mgr) = mgr();
        // Create a sparse-enabled namespace via get_or_create (sparse = true).
        let be = mgr
            .get_or_create(ORG_A, "addon_h", "docs", 4, Metric::Cosine, &[], true)
            .unwrap();
        be.upsert(
            1,
            &[1.0, 0.0, 0.0, 0.0],
            &[],
            Some(&SparseVector {
                indices: vec![100],
                values: vec![0.9],
            }),
        )
        .unwrap();
        be.upsert(
            2,
            &[0.0, 1.0, 0.0, 0.0],
            &[],
            Some(&SparseVector {
                indices: vec![300],
                values: vec![0.8],
            }),
        )
        .unwrap();

        // Hybrid query: dense near doc 1, sparse term 300 (doc 2). Both surface.
        let hits = be
            .hybrid_search(
                &[0.9, 0.1, 0.0, 0.0],
                &SparseVector {
                    indices: vec![300],
                    values: vec![1.0],
                },
                5,
                None,
                &[],
                Fusion::Rrf(60),
            )
            .unwrap();
        let ids: std::collections::HashSet<u64> = hits.iter().map(|h| h.ref_id).collect();
        assert!(ids.contains(&1) && ids.contains(&2));

        // A dense-only namespace rejects sparse upsert + hybrid search.
        let dense_only = mgr
            .get_or_create(ORG_A, "addon_h", "dense", 4, Metric::Cosine, &[], false)
            .unwrap();
        assert!(dense_only
            .upsert(
                1,
                &[1.0, 0.0, 0.0, 0.0],
                &[],
                Some(&SparseVector {
                    indices: vec![1],
                    values: vec![1.0]
                })
            )
            .is_err());
    }

    #[test]
    fn test_reconcile_namespace_noop_when_absent() {
        let (_dir, mgr) = mgr();
        // No DB row yet → reconcile is a no-op (created with new schema on first use).
        let desired = vec![FieldSpec {
            name: "x".to_string(),
            field_type: FieldType::Int,
            indexed: false,
        }];
        let report = mgr
            .reconcile_namespace(ORG_A, "addon_none", "ghost", &desired)
            .unwrap();
        assert!(report.is_noop());
    }

    #[test]
    fn test_upsert_with_quota_blocks_new_insert_at_cap() {
        let (_dir, mgr) = mgr();
        mgr.upsert_with_quota(
            ORG_A,
            "addon_a",
            "ns1",
            1,
            &[1.0, 0.0, 0.0],
            3,
            Metric::Cosine,
            &[],
            &[],
            false,
            None,
            None,
        )
        .unwrap();
        {
            let conn = mgr.pool.write().unwrap();
            conn.execute(
                "UPDATE addon_vector_namespaces SET count = ?1 \
                 WHERE addon_id = 'addon_a' AND org_id = 'org-a'",
                rusqlite::params![MAX_VECTORS_PER_ADDON as i64],
            )
            .unwrap();
        }
        let err = mgr
            .upsert_with_quota(
                ORG_A,
                "addon_a",
                "ns1",
                999,
                &[0.0, 0.0, 1.0],
                3,
                Metric::Cosine,
                &[],
                &[],
                false,
                None,
                None,
            )
            .unwrap_err();
        assert!(matches!(err, VectorError::VectorQuotaExceeded { .. }));
    }

    /// `create_dir` w scieżce quota-upsert tworzy przestrzen U SIEBIE, a nie w
    /// drzewie addonow — to jest cala mechanika, ktora pozwala jednemu flow
    /// obsluzyc wlasciciela spoza tego drzewa (projekt).
    #[test]
    fn upsert_with_quota_creates_namespace_in_custom_dir() {
        let (_dir, mgr) = mgr();
        let home = TempDir::new().unwrap();
        mgr.upsert_with_quota(
            ORG_A,
            "ps-proj1",
            "passages",
            1,
            &[0.1, 0.2, 0.3],
            3,
            Metric::Cosine,
            &[],
            &[],
            false,
            None,
            Some(home.path()),
        )
        .expect("upsert with custom dir");
        assert!(
            home.path().join("passages.usearch").exists(),
            "indeks musi powstac we wskazanym katalogu"
        );
    }

    /// Wiersz przestrzeni jest zrodlem prawdy o lokalizacji: gdy juz istnieje,
    /// pozniejszy `create_dir` jest ignorowany. Inaczej dalo by sie rozszczepic
    /// kolekcje na dwa pliki i "zgubic" zapisane wektory.
    #[test]
    fn existing_namespace_ignores_later_custom_dir() {
        let (_dir, mgr) = mgr();
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        for (i, dir) in [(1u64, first.path()), (2u64, second.path())] {
            mgr.upsert_with_quota(
                ORG_A,
                "ps-proj1",
                "passages",
                i,
                &[0.1, 0.2, 0.3],
                3,
                Metric::Cosine,
                &[],
                &[],
                false,
                None,
                Some(dir),
            )
            .expect("upsert");
        }
        assert!(first.path().join("passages.usearch").exists());
        assert!(
            !second.path().join("passages.usearch").exists(),
            "drugi katalog nie moze przejac istniejacej przestrzeni"
        );
    }

    /// Sciezka wzgledna i traversal sa odrzucane: `file_path` laduje w rejestrze
    /// na trwale, wiec `..` zapisalby indeks poza obszarem danych na stale.
    #[test]
    fn custom_dir_rejects_relative_and_traversal() {
        let (_dir, mgr) = mgr();
        for bad in [
            std::path::PathBuf::from("relative/dir"),
            std::path::PathBuf::from("/data/projects/../../etc"),
        ] {
            let err = match mgr.get_or_create_at(
                ORG_A,
                "ps-proj1",
                "passages",
                3,
                Metric::Cosine,
                &[],
                false,
                &bad,
            ) {
                Ok(_) => panic!("zly katalog musi byc odrzucony: {}", bad.display()),
                Err(e) => e,
            };
            assert!(
                format!("{err}").contains("must be absolute")
                    || format!("{err}").contains("must not contain"),
                "nieoczekiwany blad: {err}"
            );
        }
    }
}
