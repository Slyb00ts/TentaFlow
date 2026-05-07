// =============================================================================
// Plik: addon/flow_blocks.rs
// Opis: Integracja addonow z Flow Builder — parsowanie blocks.json, rejestracja
//       bloczkow addonowych w AdapterRegistry, adapter wezla addon dla DAG.
// =============================================================================

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};

// =============================================================================
// Typy bloczkow flow builder z addonu
// =============================================================================

/// Port wejsciowy lub wyjsciowy bloczka flow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockPort {
    /// Nazwa portu (np. "trigger", "message", "error")
    pub name: String,
    /// Typ danych na porcie (np. "string", "number", "boolean", "any", "json")
    #[serde(rename = "type", default = "default_port_type")]
    pub port_type: String,
    /// Czy port jest wymagany (domyslnie true dla wejsc, false dla wyjsc)
    #[serde(default)]
    pub required: bool,
}

fn default_port_type() -> String {
    "any".to_string()
}

/// Surowa definicja bloczka z blocks.json (format z pliku)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawBlockDefinition {
    /// Typ bloczka (np. "teams_send_message")
    #[serde(rename = "type")]
    block_type: String,
    /// Kategoria w palecie (np. "addon", "communication", "utility")
    #[serde(default = "default_category")]
    category: String,
    /// Etykieta wyswietlana w UI
    label: String,
    /// Opis bloczka
    #[serde(default)]
    description: String,
    /// Ikona (opcjonalna, nazwa z zestawu ikon)
    #[serde(default)]
    icon: Option<String>,
    /// JSON Schema konfiguracji bloczka
    #[serde(default)]
    config_schema: Value,
    /// Porty wejsciowe (legacy format — lista nazw string)
    #[serde(default)]
    input_ports: Vec<serde_json::Value>,
    /// Porty wyjsciowe (legacy format — lista nazw string)
    #[serde(default)]
    output_ports: Vec<serde_json::Value>,
    /// Porty wejsciowe (nowy format — obiekty BlockPort)
    #[serde(default)]
    inputs: Vec<serde_json::Value>,
    /// Porty wyjsciowe (nowy format — obiekty BlockPort)
    #[serde(default)]
    outputs: Vec<serde_json::Value>,
    /// Dodatkowa konfiguracja bloczka
    #[serde(default)]
    config: Value,
}

fn default_category() -> String {
    "addon".to_string()
}

/// Surowy plik blocks.json
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawBlocksFile {
    blocks: Vec<RawBlockDefinition>,
}

/// Definicja bloczka flow builder zarejestrowanego z addonu.
/// Przechowywany w AddonFlowRegistry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonFlowBlock {
    /// Typ bloczka w flow (format: "addon.{addon_id}.{block_type}")
    pub block_type: String,
    /// ID addonu ktory dostarczyl ten bloczek
    pub addon_id: String,
    /// Oryginalny typ bloczka z blocks.json (bez prefixu)
    pub original_type: String,
    /// Kategoria w palecie flow builder
    pub category: String,
    /// Etykieta wyswietlana w UI
    pub label: String,
    /// Opis bloczka
    pub description: String,
    /// Ikona (opcjonalna)
    pub icon: Option<String>,
    /// Porty wejsciowe
    pub inputs: Vec<BlockPort>,
    /// Porty wyjsciowe
    pub outputs: Vec<BlockPort>,
    /// JSON Schema dla konfiguracji bloczka
    pub config_schema: Value,
}

// =============================================================================
// Parsowanie blocks.json
// =============================================================================

/// Laduje i parsuje blocks.json z katalogu addonu.
/// Zwraca liste bloczkow flow builder zarejestrowanych przez addon.
pub fn load_blocks_from_addon(addon_id: &str, addon_dir: &Path) -> Result<Vec<AddonFlowBlock>> {
    let blocks_path = addon_dir.join("blocks.json");

    if !blocks_path.exists() {
        debug!(
            "Addon '{}' nie ma pliku blocks.json — brak bloczkow flow",
            addon_id
        );
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&blocks_path).with_context(|| {
        format!(
            "Nie udalo sie odczytac blocks.json dla addonu '{}'",
            addon_id
        )
    })?;

    parse_blocks_json(addon_id, &content)
}

/// Parsuje zawartosc blocks.json i zwraca liste AddonFlowBlock.
/// Obsluguje oba formaty portow: legacy (lista stringow) i nowy (obiekty).
pub fn parse_blocks_json(addon_id: &str, json_content: &str) -> Result<Vec<AddonFlowBlock>> {
    let raw: RawBlocksFile = serde_json::from_str(json_content)
        .with_context(|| format!("Niepoprawny format blocks.json dla addonu '{}'", addon_id))?;

    let mut blocks = Vec::with_capacity(raw.blocks.len());

    for raw_block in raw.blocks {
        // Parsuj porty wejsciowe — obsluga obu formatow
        let inputs = parse_ports(&raw_block.inputs, &raw_block.input_ports, true);
        let outputs = parse_ports(&raw_block.outputs, &raw_block.output_ports, false);

        // Typ bloczka z prefixem addon_id (unikalne w flow engine)
        let block_type = format!("addon.{}.{}", addon_id, raw_block.block_type);

        blocks.push(AddonFlowBlock {
            block_type,
            addon_id: addon_id.to_string(),
            original_type: raw_block.block_type,
            category: raw_block.category,
            label: raw_block.label,
            description: raw_block.description,
            icon: raw_block.icon,
            inputs,
            outputs,
            config_schema: if raw_block.config_schema.is_null() {
                raw_block.config
            } else {
                raw_block.config_schema
            },
        });
    }

    info!(
        "Zaladowano {} bloczkow flow z addonu '{}'",
        blocks.len(),
        addon_id
    );

    Ok(blocks)
}

/// Parsuje porty — obsluguje format nowy (obiekty BlockPort) i legacy (lista stringow).
fn parse_ports(
    new_format: &[serde_json::Value],
    legacy_format: &[serde_json::Value],
    is_input: bool,
) -> Vec<BlockPort> {
    // Preferuj nowy format
    let source = if !new_format.is_empty() {
        new_format
    } else {
        legacy_format
    };

    source
        .iter()
        .filter_map(|val| {
            match val {
                // Nowy format: obiekt z polami name, type, required
                Value::Object(obj) => {
                    let name = obj
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unnamed")
                        .to_string();
                    let port_type = obj
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("any")
                        .to_string();
                    let required = obj
                        .get("required")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(is_input);
                    Some(BlockPort {
                        name,
                        port_type,
                        required,
                    })
                }
                // Legacy format: sam string z nazwa portu
                Value::String(name) => Some(BlockPort {
                    name: name.clone(),
                    port_type: "any".to_string(),
                    required: is_input,
                }),
                _ => {
                    warn!("Nieobslugiwany format portu: {:?}", val);
                    None
                }
            }
        })
        .collect()
}

// =============================================================================
// AddonFlowRegistry — rejestr bloczkow addonowych
// =============================================================================

/// Rejestr bloczkow flow builder dostarczanych przez addony.
/// Centralny punkt do odpytywania o dostepne bloczki.
pub struct AddonFlowRegistry {
    /// Wszystkie zarejestrowane bloczki (klucz: block_type z prefixem)
    blocks: parking_lot::RwLock<Vec<AddonFlowBlock>>,
}

impl AddonFlowRegistry {
    /// Tworzy pusty rejestr
    pub fn new() -> Self {
        Self {
            blocks: parking_lot::RwLock::new(Vec::new()),
        }
    }

    /// Rejestruje bloczki z addonu (podmienia jesli addon byl juz zarejestrowany)
    pub fn register_addon_blocks(&self, addon_id: &str, blocks: Vec<AddonFlowBlock>) {
        let mut all_blocks = self.blocks.write();

        // Usun stare bloczki tego addonu
        all_blocks.retain(|b| b.addon_id != addon_id);

        let count = blocks.len();
        all_blocks.extend(blocks);

        info!(
            "Zarejestrowano {} bloczkow flow z addonu '{}'",
            count, addon_id
        );
    }

    /// Wyrejestrowuje wszystkie bloczki addonu
    pub fn unregister_addon_blocks(&self, addon_id: &str) {
        let mut blocks = self.blocks.write();
        let before = blocks.len();
        blocks.retain(|b| b.addon_id != addon_id);
        let removed = before - blocks.len();
        if removed > 0 {
            info!(
                "Wyrejestrowano {} bloczkow flow addonu '{}'",
                removed, addon_id
            );
        }
    }

    /// Zwraca wszystkie zarejestrowane bloczki (kopia)
    pub fn list_all_blocks(&self) -> Vec<AddonFlowBlock> {
        self.blocks.read().clone()
    }

    /// Zwraca bloczki z konkretnego addonu
    pub fn list_blocks_for_addon(&self, addon_id: &str) -> Vec<AddonFlowBlock> {
        self.blocks
            .read()
            .iter()
            .filter(|b| b.addon_id == addon_id)
            .cloned()
            .collect()
    }

    /// Znajduje bloczek po pelnym typie (np. "addon.teams.send_message")
    pub fn find_block(&self, block_type: &str) -> Option<AddonFlowBlock> {
        self.blocks
            .read()
            .iter()
            .find(|b| b.block_type == block_type)
            .cloned()
    }

    /// Zwraca bloczki pogrupowane po kategorii (dla UI flow builder)
    pub fn blocks_by_category(&self) -> std::collections::HashMap<String, Vec<AddonFlowBlock>> {
        let blocks = self.blocks.read();
        let mut groups: std::collections::HashMap<String, Vec<AddonFlowBlock>> =
            std::collections::HashMap::new();
        for block in blocks.iter() {
            groups
                .entry(block.category.clone())
                .or_default()
                .push(block.clone());
        }
        groups
    }

    /// Serializuje bloczki do formatu JSON dla frontendu flow builder
    pub fn to_flow_builder_json(&self) -> serde_json::Value {
        let blocks = self.blocks.read();
        let json_blocks: Vec<Value> = blocks
            .iter()
            .map(|b| {
                serde_json::json!({
                    "type": b.block_type,
                    "addon_id": b.addon_id,
                    "category": b.category,
                    "label": b.label,
                    "description": b.description,
                    "icon": b.icon,
                    "inputs": b.inputs.iter().map(|p| serde_json::json!({
                        "name": p.name,
                        "type": p.port_type,
                        "required": p.required,
                    })).collect::<Vec<_>>(),
                    "outputs": b.outputs.iter().map(|p| serde_json::json!({
                        "name": p.name,
                        "type": p.port_type,
                    })).collect::<Vec<_>>(),
                    "config_schema": b.config_schema,
                })
            })
            .collect();

        serde_json::json!({
            "addon_blocks": json_blocks
        })
    }
}

impl Default for AddonFlowRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_blocks_json_basic() {
        let json = r#"{
            "blocks": [
                {
                    "type": "hello",
                    "category": "utility",
                    "label": "Powitanie",
                    "description": "Zwraca powitanie",
                    "inputs": [],
                    "outputs": [
                        {"name": "message", "type": "string"}
                    ],
                    "config": {}
                }
            ]
        }"#;

        let blocks = parse_blocks_json("test_addon", json).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_type, "addon.test_addon.hello");
        assert_eq!(blocks[0].addon_id, "test_addon");
        assert_eq!(blocks[0].original_type, "hello");
        assert_eq!(blocks[0].category, "utility");
        assert_eq!(blocks[0].label, "Powitanie");
        assert_eq!(blocks[0].inputs.len(), 0);
        assert_eq!(blocks[0].outputs.len(), 1);
        assert_eq!(blocks[0].outputs[0].name, "message");
        assert_eq!(blocks[0].outputs[0].port_type, "string");
    }

    #[test]
    fn test_parse_blocks_json_legacy_ports() {
        let json = r#"{
            "blocks": [
                {
                    "type": "send",
                    "label": "Wyslij",
                    "input_ports": ["input"],
                    "output_ports": ["success", "error"],
                    "config_schema": {"type": "object"}
                }
            ]
        }"#;

        let blocks = parse_blocks_json("teams", json).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].inputs.len(), 1);
        assert_eq!(blocks[0].inputs[0].name, "input");
        assert_eq!(blocks[0].inputs[0].port_type, "any");
        assert_eq!(blocks[0].inputs[0].required, true);
        assert_eq!(blocks[0].outputs.len(), 2);
        assert_eq!(blocks[0].outputs[0].name, "success");
        assert_eq!(blocks[0].outputs[0].required, false);
    }

    #[test]
    fn test_parse_blocks_json_invalid() {
        let result = parse_blocks_json("broken", "not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_addon_flow_registry() {
        let registry = AddonFlowRegistry::new();

        let blocks = vec![AddonFlowBlock {
            block_type: "addon.test.hello".to_string(),
            addon_id: "test".to_string(),
            original_type: "hello".to_string(),
            category: "utility".to_string(),
            label: "Hello".to_string(),
            description: "Test".to_string(),
            icon: None,
            inputs: vec![],
            outputs: vec![BlockPort {
                name: "msg".to_string(),
                port_type: "string".to_string(),
                required: false,
            }],
            config_schema: serde_json::json!({}),
        }];

        registry.register_addon_blocks("test", blocks);

        assert_eq!(registry.list_all_blocks().len(), 1);
        assert!(registry.find_block("addon.test.hello").is_some());
        assert!(registry.find_block("addon.test.nonexistent").is_none());

        // Wyrejestruj
        registry.unregister_addon_blocks("test");
        assert_eq!(registry.list_all_blocks().len(), 0);
    }
}
