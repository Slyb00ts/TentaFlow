// =============================================================================
// Plik: registry.rs
// Opis: Globalny rejestr (ManifestRegistry) zaladowanych manifestow silnikow.
//       Inicjalizowany leniwie z embeddowanego JSON wygenerowanego przez build.rs.
// =============================================================================

use super::types::*;
use std::sync::OnceLock;

// Plik wygenerowany przez build.rs — eksportuje GENERATED_MANIFEST_JSON: &str.
include!(concat!(env!("OUT_DIR"), "/services_generated.rs"));

/// Rejestr manifestow silnikow — agregat z wszystkich plikow `_services/*.toml`.
pub struct ManifestRegistry {
    engines: Vec<ServiceManifest>,
}

impl ManifestRegistry {
    /// Wszystkie zarejestrowane silniki.
    pub fn engines(&self) -> &[ServiceManifest] {
        &self.engines
    }

    /// Wyszukuje silnik po `engine.id`.
    pub fn by_id(&self, id: &str) -> Option<&ServiceManifest> {
        self.engines.iter().find(|e| e.engine.id == id)
    }

    /// Zwraca silniki z danej kategorii (uwzglednia takze `also_serves`).
    pub fn by_category(&self, cat: Category) -> Vec<&ServiceManifest> {
        self.engines
            .iter()
            .filter(|e| e.engine.category == cat || e.engine.also_serves.contains(&cat))
            .collect()
    }

    /// Silniki, ktorych jakikolwiek wariant jest zgodny z platforma docelowa.
    /// Wariant z `target_arch = any` pasuje do dowolnej architektury.
    pub fn compatible_for(
        &self,
        os: TargetOs,
        arch: TargetArch,
        gpu: GpuBackend,
    ) -> Vec<&ServiceManifest> {
        self.engines
            .iter()
            .filter(|m| {
                m.variants.iter().any(|v| {
                    let arch_list = v.target_arch.as_vec();
                    v.target_os.as_vec().contains(&os)
                        && (arch_list.contains(&arch) || arch_list.contains(&TargetArch::Any))
                        && v.gpu_backend.as_vec().contains(&gpu)
                })
            })
            .collect()
    }
}

static REGISTRY_CELL: OnceLock<ManifestRegistry> = OnceLock::new();

/// Globalny singleton rejestru — leniwa inicjalizacja przy pierwszym dostepie.
pub fn registry() -> &'static ManifestRegistry {
    REGISTRY_CELL.get_or_init(|| {
        let engines: Vec<ServiceManifest> = serde_json::from_str(GENERATED_MANIFEST_JSON)
            .expect("GENERATED_MANIFEST_JSON powinien byc poprawny — bug w build.rs");
        ManifestRegistry { engines }
    })
}
