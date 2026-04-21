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

    /// Zwraca silniki z danej kategorii.
    pub fn by_category(&self, cat: Category) -> Vec<&ServiceManifest> {
        self.engines
            .iter()
            .filter(|e| e.engine.category == cat)
            .collect()
    }

    /// Silniki, ktore maja chociaz jedna sekcje deploy z platforma `os` na liscie.
    /// Bez sprawdzania architektury / GPU — silnik runtime sam wykrywa zasoby hosta.
    pub fn compatible_for(&self, os: TargetOs) -> Vec<&ServiceManifest> {
        self.engines
            .iter()
            .filter(|m| {
                let d = &m.deploy;
                d.docker.as_ref().is_some_and(|x| x.platforms.contains(&os))
                    || d.native.as_ref().is_some_and(|x| x.platforms.contains(&os))
                    || d.external
                        .as_ref()
                        .is_some_and(|x| x.platforms.contains(&os))
            })
            .collect()
    }

    /// Lista kategorii, ktore zawieraja przynajmniej jeden silnik. Sluzy do
    /// auto-ukrywania pustych sekcji w GUI (kategoria bez plikow TOML = nie wyswietlana).
    pub fn non_empty_categories(&self) -> Vec<Category> {
        let mut seen: Vec<Category> = Vec::new();
        for e in &self.engines {
            if !seen.contains(&e.engine.category) {
                seen.push(e.engine.category);
            }
        }
        seen
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
