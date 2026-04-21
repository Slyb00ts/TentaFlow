// =============================================================================
// Plik: addon/bundled.rs
// Opis: Obsluga wbudowanych addonow — osadzonych w binarce przez build.rs.
//       Automatycznie instaluje lub aktualizuje bundled addony przy starcie
//       aplikacji (Router, Desktop, Mobile).
// =============================================================================

use std::path::PathBuf;

use anyhow::Result;
use sha2::{Digest, Sha256};
use tracing::{error, info};

use crate::db::DbPool;

// Wlacz wygenerowany plik z osadzonymi addonami
include!(concat!(env!("OUT_DIR"), "/bundled_addons.rs"));

const BUNDLE_HASH_SETTING_PREFIX: &str = "addon_bundle_hash:";

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
        BUNDLED_ADDONS
            .iter()
            .map(|a| a.wasm_bytes.len())
            .sum::<usize>()
    );

    let data_dir = bundled_addons_dir();
    std::fs::create_dir_all(&data_dir).map_err(|e| {
        anyhow::anyhow!("Nie udalo sie utworzyc katalogu dla wbudowanych addonow: {e}")
    })?;

    for addon in BUNDLED_ADDONS {
        if let Err(e) = install_single_bundled_addon(addon, db, &data_dir) {
            error!("Blad instalacji wbudowanego addonu '{}': {}", addon.name, e);
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
            return Err(anyhow::anyhow!(
                "Nie udalo sie sparsowac manifest.toml: {e}"
            ));
        }
    };

    let bundle_hash = compute_bundle_hash(addon);

    // Sprawdz czy addon juz istnieje w DB
    let existing = crate::db::repository::get_addon(db, &addon_id)?;
    let stored_bundle_hash =
        crate::db::repository::get_setting(db, &bundle_hash_setting_key(&addon_id))?;

    match existing {
        Some(ref existing_addon)
            if existing_addon.version == bundled_version
                && existing_addon.manifest_json == addon.manifest_toml
                && stored_bundle_hash.as_deref() == Some(bundle_hash.as_str()) =>
        {
            info!(
                "Wbudowany addon '{}' v{} jest aktualny — pomijam",
                addon_id, bundled_version
            );
            return Ok(());
        }
        Some(ref existing_addon) => {
            let mut reasons: Vec<&str> = Vec::new();
            if existing_addon.version != bundled_version {
                reasons.push("version");
            }
            if existing_addon.manifest_json != addon.manifest_toml {
                reasons.push("manifest");
            }
            if stored_bundle_hash.as_deref() != Some(bundle_hash.as_str()) {
                reasons.push("bundle_hash");
            }
            info!(
                "Aktualizacja wbudowanego addonu '{}': v{} -> v{} (powod: {})",
                addon_id,
                existing_addon.version,
                bundled_version,
                reasons.join(", ")
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
        crate::db::repository::set_setting(db, &bundle_hash_setting_key(&addon_id), &bundle_hash)?;
        info!(
            "Wbudowany addon '{}' zaktualizowany do v{}",
            addon_id, bundled_version
        );
    } else {
        super::lifecycle::install(&addon_dir, db)?;
        crate::db::repository::set_setting(db, &bundle_hash_setting_key(&addon_id), &bundle_hash)?;
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

fn bundle_hash_setting_key(addon_id: &str) -> String {
    format!("{BUNDLE_HASH_SETTING_PREFIX}{addon_id}")
}

fn compute_bundle_hash(addon: &BundledAddon) -> String {
    let mut hasher = Sha256::new();
    hash_chunk(&mut hasher, b"addon.wasm", addon.wasm_bytes);
    hash_chunk(
        &mut hasher,
        b"manifest.toml",
        addon.manifest_toml.as_bytes(),
    );
    hash_chunk(&mut hasher, b"SKILL.md", addon.skill_md.as_bytes());
    hash_chunk(
        &mut hasher,
        b"DESCRIPTION.md",
        addon.description_md.as_bytes(),
    );
    hash_chunk(&mut hasher, b"blocks.json", addon.blocks_json.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn hash_chunk(hasher: &mut Sha256, name: &[u8], bytes: &[u8]) {
    hasher.update((name.len() as u64).to_le_bytes());
    hasher.update(name);
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
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
    fn test_bundle_hash_changes_when_manifest_changes() {
        let addon_a = BundledAddon {
            name: "outlook",
            wasm_bytes: &[1, 2, 3],
            manifest_toml: "[addon]\nid=\"outlook\"\nversion=\"0.1.0\"\n",
            skill_md: "",
            description_md: "",
            blocks_json: "",
        };
        let addon_b = BundledAddon {
            name: "outlook",
            wasm_bytes: &[1, 2, 3],
            manifest_toml: "[addon]\nid=\"outlook\"\nversion=\"0.1.1\"\n",
            skill_md: "",
            description_md: "",
            blocks_json: "",
        };

        assert_ne!(compute_bundle_hash(&addon_a), compute_bundle_hash(&addon_b));
    }

    #[test]
    fn test_bundle_hash_changes_when_wasm_changes() {
        let addon_a = BundledAddon {
            name: "outlook",
            wasm_bytes: &[1, 2, 3],
            manifest_toml: "[addon]\nid=\"outlook\"\nversion=\"0.1.0\"\n",
            skill_md: "",
            description_md: "",
            blocks_json: "",
        };
        let addon_b = BundledAddon {
            name: "outlook",
            wasm_bytes: &[1, 2, 4],
            manifest_toml: "[addon]\nid=\"outlook\"\nversion=\"0.1.0\"\n",
            skill_md: "",
            description_md: "",
            blocks_json: "",
        };

        assert_ne!(compute_bundle_hash(&addon_a), compute_bundle_hash(&addon_b));
    }

    #[test]
    fn test_bundled_addons_constant_exists() {
        // Sprawdz ze stala BUNDLED_ADDONS jest dostepna
        let _ = BUNDLED_ADDONS.len();
    }

    /// Every bundled manifest parses cleanly in the canonical format and
    /// declares at least one permission with a valid risk level and non-empty
    /// display name. Guards against manifests drifting from the format.
    #[test]
    fn bundled_manifests_use_canonical_format() {
        use crate::addon::lifecycle::parse_manifest_toml;

        const VALID_RISK: &[&str] = &["low", "medium", "high", "critical"];

        assert!(!BUNDLED_ADDONS.is_empty(), "no bundled addons to validate");

        for addon in BUNDLED_ADDONS {
            let manifest = parse_manifest_toml(addon.manifest_toml)
                .unwrap_or_else(|e| panic!("manifest parse failed for '{}': {}", addon.name, e));

            assert!(
                !manifest.declared_permissions.is_empty(),
                "addon '{}' declares no permissions",
                manifest.addon_id
            );

            for perm in &manifest.declared_permissions {
                assert!(
                    !perm.id.is_empty(),
                    "addon '{}' has empty permission id",
                    manifest.addon_id
                );
                assert!(
                    !perm.display_name.is_empty(),
                    "addon '{}' permission '{}' has empty display_name",
                    manifest.addon_id,
                    perm.id
                );
                assert!(
                    VALID_RISK.contains(&perm.risk.as_str()),
                    "addon '{}' permission '{}' has invalid risk '{}'",
                    manifest.addon_id,
                    perm.id,
                    perm.risk
                );
            }
        }
    }
}
