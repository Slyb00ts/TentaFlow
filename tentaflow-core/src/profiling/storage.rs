// =============================================================================
// Plik: profiling/storage.rs
// Opis: FIFO storage sesji Nsight per nod — alokacja katalogu sesji, zapis
//       summary.bin (rkyv ProfileReport), listing, rotacja MAX 20 sesji,
//       walidacja path traversal po session_id.
// =============================================================================

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use std::sync::LazyLock;
use tentaflow_protocol::profiling::{
    NsightScope, NsightSessionEntry, NsightSessionStatus, ProfileReport,
};

use super::nsys::ProfilingError;

/// Maksymalna liczba sesji trzymana per nod — najstarsze usuwane przez `rotate()`.
pub const MAX_SESSIONS_PER_NODE: usize = 20;

/// Regex dla session_id — UUIDv4 simple (32 hex), z minimum 16 dla testow.
/// Tylko lowercase hex, blokuje "..", "/" i wszystko poza alfabetem hex.
static SESSION_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-f0-9]{16,32}$").expect("valid session id regex"));

fn validate_session_id(s: &str) -> Result<(), ProfilingError> {
    if SESSION_ID_RE.is_match(s) {
        Ok(())
    } else {
        Err(ProfilingError::InvalidSessionId)
    }
}

/// Storage sesji — root layout: `<root>/<node_id>/<session_id>/{report.nsys-rep, summary.bin}`.
pub struct ProfileStorage {
    root: PathBuf,
    node_id: String,
}

impl ProfileStorage {
    /// `data_dir` to katalog danych aplikacji (np. `tentaflow_home`); pod nim
    /// powstaje `nsight/<node_id>/`.
    pub fn new(data_dir: &Path, node_id: &str) -> Self {
        Self {
            root: data_dir.join("nsight"),
            node_id: node_id.to_string(),
        }
    }

    fn node_dir(&self) -> PathBuf {
        self.root.join(&self.node_id)
    }

    fn session_dir(&self, session_id: &str) -> Result<PathBuf, ProfilingError> {
        validate_session_id(session_id)?;
        Ok(self.node_dir().join(session_id))
    }

    /// Alokuje nowy katalog sesji i zwraca `(session_id, scieżka do .nsys-rep)`.
    /// `.nsys-rep` jeszcze nie istnieje — zostanie utworzony przez `nsys profile`.
    pub fn allocate(
        &self,
        _label: &str,
        _scope: &NsightScope,
    ) -> Result<(String, PathBuf), ProfilingError> {
        // UUIDv4 simple: 32 lowercase hex bez kresek — zgodny z naszym regexem.
        let session_id = uuid::Uuid::new_v4().simple().to_string();
        let dir = self.session_dir(&session_id)?;
        fs::create_dir_all(&dir)?;
        let rep_path = dir.join("report.nsys-rep");
        Ok((session_id, rep_path))
    }

    /// Pelna sciezka do pliku `.nsys-rep` po walidacji session_id.
    pub fn raw_report_path(&self, session_id: &str) -> Result<PathBuf, ProfilingError> {
        let dir = self.session_dir(session_id)?;
        Ok(dir.join("report.nsys-rep"))
    }

    /// Zapis raportu rkyv do `<session>/summary.bin`.
    pub fn write_summary(
        &self,
        session_id: &str,
        report: &ProfileReport,
    ) -> Result<(), ProfilingError> {
        let dir = self.session_dir(session_id)?;
        fs::create_dir_all(&dir)?;
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(report)
            .map_err(|e| ProfilingError::Parse(format!("rkyv encode: {e}")))?;
        fs::write(dir.join("summary.bin"), bytes.as_ref())?;
        Ok(())
    }

    /// Odczyt raportu rkyv z `<session>/summary.bin`.
    pub fn read_summary(&self, session_id: &str) -> Result<ProfileReport, ProfilingError> {
        let dir = self.session_dir(session_id)?;
        let bytes = fs::read(dir.join("summary.bin"))?;
        rkyv::from_bytes::<ProfileReport, rkyv::rancor::Error>(&bytes)
            .map_err(|e| ProfilingError::Parse(format!("rkyv decode: {e}")))
    }

    /// Lista sesji posortowana desc po `started_at_ms`. Sesje bez `summary.bin`
    /// (np. w trakcie nagrywania albo przerwane) sa pominiete w listingu.
    pub fn list(&self) -> Result<Vec<NsightSessionEntry>, ProfilingError> {
        let dir = self.node_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries: Vec<NsightSessionEntry> = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if validate_session_id(&name).is_err() {
                continue;
            }
            let session_dir = entry.path();
            let summary_path = session_dir.join("summary.bin");
            if !summary_path.exists() {
                continue;
            }
            let report = match self.read_summary(&name) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let rep_size = fs::metadata(session_dir.join("report.nsys-rep"))
                .map(|m| m.len())
                .unwrap_or(0);
            entries.push(NsightSessionEntry {
                session_id: name,
                label: report.meta.label,
                scope: report.meta.scope,
                status: NsightSessionStatus::Done,
                started_at_ms: report.meta.started_at_ms,
                duration_ms: report.meta.duration_ms,
                size_bytes: rep_size,
                error: None,
            });
        }

        entries.sort_by(|a, b| b.started_at_ms.cmp(&a.started_at_ms));
        Ok(entries)
    }

    /// Usuwa katalog konkretnej sesji. Wykonuje canonicalize i sprawdza, ze
    /// rozwiazana sciezka nadal jest pod `<root>/<node_id>/` (ochrona przed
    /// ewentualnym path traversal jezeli regex sie kiedys popsuje).
    pub fn delete(&self, session_id: &str) -> Result<(), ProfilingError> {
        let dir = self.session_dir(session_id)?;
        if !dir.exists() {
            return Err(ProfilingError::NotFound(session_id.to_string()));
        }
        let canon = fs::canonicalize(&dir)?;
        let node_canon = fs::canonicalize(self.node_dir())?;
        if !canon.starts_with(&node_canon) {
            return Err(ProfilingError::InvalidSessionId);
        }
        fs::remove_dir_all(&canon)?;
        Ok(())
    }

    /// Usuwa najstarsze sesje powyzej limitu `MAX_SESSIONS_PER_NODE`. Jako
    /// kryterium wieku uzywa `started_at_ms` zapisanego w summary.bin.
    pub fn rotate(&self) -> Result<(), ProfilingError> {
        let entries = self.list()?;
        if entries.len() <= MAX_SESSIONS_PER_NODE {
            return Ok(());
        }
        // entries sa desc po started_at_ms — najstarsze sa na koncu.
        for old in entries.iter().skip(MAX_SESSIONS_PER_NODE) {
            let _ = self.delete(&old.session_id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::profiling::{
        NsightScope, ProfileKpi, ProfileMeta, ProfileReport,
    };

    fn dummy_report(session_id: &str, started_at_ms: u64) -> ProfileReport {
        ProfileReport {
            meta: ProfileMeta {
                session_id: session_id.to_string(),
                label: "t".to_string(),
                scope: NsightScope::Cpu,
                hostname: "h".to_string(),
                started_at_ms,
                duration_ms: 100,
                nsys_version: "0".to_string(),
                gpu_targets: Vec::new(),
            },
            kpi: ProfileKpi::default(),
            gpu_kernels_top: Vec::new(),
            cuda_api_top: Vec::new(),
            gpu_mem_ops: Vec::new(),
            cpu_samples_top: Vec::new(),
            nvtx_ranges_top: Vec::new(),
            gpu_util_timeline: Vec::new(),
        }
    }

    fn make_storage() -> (tempfile::TempDir, ProfileStorage) {
        let tmp = tempfile::tempdir().unwrap();
        let st = ProfileStorage::new(tmp.path(), "node-test");
        (tmp, st)
    }

    #[test]
    fn storage_session_id_traversal() {
        let (_tmp, st) = make_storage();
        let err = st.delete("../../../etc/passwd").unwrap_err();
        assert!(matches!(err, ProfilingError::InvalidSessionId));
    }

    #[test]
    fn storage_session_id_dotdot() {
        let (_tmp, st) = make_storage();
        let err = st.read_summary("..").unwrap_err();
        assert!(matches!(err, ProfilingError::InvalidSessionId));
    }

    #[test]
    fn storage_session_id_valid_uuid() {
        let id = uuid::Uuid::new_v4().simple().to_string();
        assert!(validate_session_id(&id).is_ok());
    }

    #[test]
    fn storage_session_id_too_short() {
        let err = validate_session_id("abc12345").unwrap_err();
        assert!(matches!(err, ProfilingError::InvalidSessionId));
    }

    #[test]
    fn storage_session_id_uppercase() {
        let err = validate_session_id("ABCDEF0123456789abcdef0123456789").unwrap_err();
        assert!(matches!(err, ProfilingError::InvalidSessionId));
    }

    #[test]
    fn storage_write_read_summary_round_trip() {
        let (_tmp, st) = make_storage();
        let (sid, _path) = st.allocate("lbl", &NsightScope::Cpu).unwrap();
        let rep = dummy_report(&sid, 1234);
        st.write_summary(&sid, &rep).unwrap();
        let read = st.read_summary(&sid).unwrap();
        assert_eq!(read, rep);
    }

    #[test]
    fn storage_fifo_rotation() {
        let (_tmp, st) = make_storage();
        // Tworz 25 sesji z rosnacym started_at_ms zeby kolejnosc desc byla deterministyczna.
        let mut ids = Vec::new();
        for i in 0..25 {
            let (sid, _p) = st.allocate("x", &NsightScope::Cpu).unwrap();
            let rep = dummy_report(&sid, 1_000 + i as u64);
            st.write_summary(&sid, &rep).unwrap();
            ids.push(sid);
        }

        let listing = st.list().unwrap();
        assert_eq!(listing.len(), 25);

        st.rotate().unwrap();
        let after = st.list().unwrap();
        assert_eq!(after.len(), MAX_SESSIONS_PER_NODE);

        // 5 najstarszych (i=0..5, started_at_ms=1000..1005) powinno byc usunietych.
        for sid in &ids[0..5] {
            assert!(st.read_summary(sid).is_err());
        }
    }
}
