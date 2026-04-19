// =============================================================================
// Plik: addon/bundled.rs
// Opis: Obsluga wbudowanych addonow — osadzonych w binarce przez build.rs.
//       Automatycznie instaluje lub aktualizuje bundled addony przy starcie
//       aplikacji (Router, Desktop, Mobile).
// =============================================================================

use std::path::PathBuf;

use anyhow::Result;
use tracing::{info, error};

use crate::db::DbPool;

// Wlacz wygenerowany plik z osadzonymi addonami
include!(concat!(env!("OUT_DIR"), "/bundled_addons.rs"));

// =============================================================================
// Instalacja wbudowanych addonow
// =============================================================================

/// Instaluje wszystkie wbudowane addony (z binarki) jesli nie sa jeszcze
/// zainstalowane lub wymagaja aktualizacji.
///
/// Kroki dla kazdego bundled addonu:
/// 1. Parsuj manifest.toml — wyciagnij addon_id i wersje
/// 2. Sprawdz czy addon juz istnieje w DB
/// 3. Jesli nie — rozpakuj do katalogu tymczasowego i zainstaluj przez lifecycle
/// 4. Jesli tak i wersja jest nowsza — rozpakuj i upgrade przez lifecycle
/// 5. Jesli ta sama wersja — pomin
pub fn install_bundled_addons(db: &DbPool) -> Result<()> {
    if BUNDLED_ADDONS.is_empty() {
        info!("Brak wbudowanych addonow do zainstalowania");
        return Ok(());
    }

    info!(
        "Sprawdzanie {} wbudowanych addonow (WASM total: {} bytes)...",
        BUNDLED_ADDONS.len(),
        BUNDLED_ADDONS.iter().map(|a| a.wasm_bytes.len()).sum::<usize>()
    );

    let data_dir = bundled_addons_dir();
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc katalogu dla wbudowanych addonow: {e}"))?;

    for addon in BUNDLED_ADDONS {
        if let Err(e) = install_single_bundled_addon(addon, db, &data_dir) {
            error!(
                "Blad instalacji wbudowanego addonu '{}': {}",
                addon.name, e
            );
            // Kontynuuj z nastepnym addonem — nie przerywaj calego procesu
        }
    }

    Ok(())
}

/// Instaluje pojedynczy wbudowany addon
fn install_single_bundled_addon(
    addon: &BundledAddon,
    db: &DbPool,
    data_dir: &std::path::Path,
) -> Result<()> {
    // Parsuj manifest — wyciagnij addon_id i wersje
    let (addon_id, bundled_version) = match parse_addon_id_and_version(addon.manifest_toml) {
        Ok(v) => v,
        Err(e) => {
            error!("Nie udalo sie sparsowac manifest.toml dla '{}': {}\nManifest (pierwsze 200 znakow): {}", addon.name, e, &addon.manifest_toml[..addon.manifest_toml.len().min(200)]);
            return Err(anyhow::anyhow!("Nie udalo sie sparsowac manifest.toml: {e}"));
        }
    };

    // Sprawdz czy addon juz istnieje w DB
    let existing = crate::db::repository::get_addon(db, &addon_id)?;

    match existing {
        Some(ref existing_addon) if existing_addon.version == bundled_version => {
            // Ta sama wersja — pomin
            info!(
                "Wbudowany addon '{}' v{} juz zainstalowany — pomijam",
                addon_id, bundled_version
            );
            return Ok(());
        }
        Some(ref existing_addon) => {
            // Inna wersja — upgrade
            info!(
                "Aktualizacja wbudowanego addonu '{}': v{} -> v{}",
                addon_id, existing_addon.version, bundled_version
            );
        }
        None => {
            info!(
                "Instalacja wbudowanego addonu '{}' v{}",
                addon_id, bundled_version
            );
        }
    }

    // Rozpakuj addon do katalogu tymczasowego
    let addon_dir = data_dir.join(&addon_id);
    std::fs::create_dir_all(&addon_dir)?;

    // Zapisz pliki
    std::fs::write(addon_dir.join("addon.wasm"), addon.wasm_bytes)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie zapisac pliku WASM: {e}"))?;

    std::fs::write(addon_dir.join("manifest.toml"), addon.manifest_toml)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie zapisac manifest.toml: {e}"))?;

    if !addon.skill_md.is_empty() {
        std::fs::write(addon_dir.join("SKILL.md"), addon.skill_md).ok();
    }
    if !addon.description_md.is_empty() {
        std::fs::write(addon_dir.join("DESCRIPTION.md"), addon.description_md).ok();
    }
    if !addon.blocks_json.is_empty() {
        std::fs::write(addon_dir.join("blocks.json"), addon.blocks_json).ok();
    }

    // Zainstaluj lub upgrade przez lifecycle
    if existing.is_some() {
        super::lifecycle::upgrade(&addon_id, &addon_dir, db)?;
        info!(
            "Wbudowany addon '{}' zaktualizowany do v{}",
            addon_id, bundled_version
        );
    } else {
        super::lifecycle::install(&addon_dir, db)?;
        info!(
            "Wbudowany addon '{}' v{} zainstalowany pomyslnie",
            addon_id, bundled_version
        );
    }

    Ok(())
}

// =============================================================================
// Parsowanie manifestu — minimalne wyciagniecie addon_id i version
// =============================================================================

/// Parsuje manifest.toml i zwraca (addon_id, version).
/// Obsluguje dwa formaty manifestu:
/// - Nowy: [addon] id = "..." version = "..."
/// - Stary: addon_id = "..." version = "..."
fn parse_addon_id_and_version(manifest_toml: &str) -> Result<(String, String)> {
    let parsed: toml::Value = toml::from_str(manifest_toml)
        .map_err(|e| anyhow::anyhow!("Niepoprawny format manifest.toml: {e}"))?;

    // Nowy format: [addon] id = "...", version = "..."
    if let Some(addon_section) = parsed.get("addon") {
        let id = addon_section
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Brak pola addon.id w manifest.toml"))?;

        let version = addon_section
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Brak pola addon.version w manifest.toml"))?;

        return Ok((id.to_string(), version.to_string()));
    }

    // Stary format: addon_id = "...", version = "..."
    let id = parsed
        .get("addon_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Brak pola addon_id ani addon.id w manifest.toml"))?;

    let version = parsed
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Brak pola version w manifest.toml"))?;

    Ok((id.to_string(), version.to_string()))
}

// =============================================================================
// Sciezka do katalogu wbudowanych addonow
// =============================================================================

/// Zwraca sciezke do katalogu gdzie rozpakowane sa wbudowane addony
fn bundled_addons_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tentaflow-ai")
        .join("bundled-addons")
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_new_format_manifest() {
        let manifest = r#"
[addon]
id = "teams"
name = "Microsoft Teams"
version = "0.1.0"
"#;
        let (id, version) = parse_addon_id_and_version(manifest).unwrap();
        assert_eq!(id, "teams");
        assert_eq!(version, "0.1.0");
    }

    #[test]
    fn test_parse_old_format_manifest() {
        let manifest = r#"
addon_id = "old-addon"
version = "1.2.3"
display_name = "Old Addon"
"#;
        let (id, version) = parse_addon_id_and_version(manifest).unwrap();
        assert_eq!(id, "old-addon");
        assert_eq!(version, "1.2.3");
    }

    #[test]
    fn test_parse_invalid_manifest_fails() {
        let manifest = "[addon]\nname = \"no-id\"";
        assert!(parse_addon_id_and_version(manifest).is_err());
    }

    #[test]
    fn test_bundled_addons_constant_exists() {
        // Sprawdz ze stala BUNDLED_ADDONS jest dostepna
        let _ = BUNDLED_ADDONS.len();
    }
}
