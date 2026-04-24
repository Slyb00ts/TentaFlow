// =============================================================================
// Plik: teams_bot_bootstrap.rs
// Opis: Idempotentna inicjalizacja domyślnych aliasów modeli i flow dla teams-bota.
// =============================================================================

use anyhow::Result;
use serde_json::json;

use crate::db::models::FlowParams;
use crate::db::{repository, DbPool};

/// Nazwy aliasów używanych przez teams-bota — pusty `target_model` sygnalizuje
/// że user powinien przypisać konkretny model w UI.
const TEAMS_ALIASES: &[&str] = &["teams-stt", "teams-summarization", "teams-tts"];

/// Nazwa domyślnego flow dla teams-bota.
const TEAMS_FLOW_NAME: &str = "teams-flow";

/// Tworzy (jeśli brak) domyślne aliasy i flow dla teams-bota. Bezpieczna do
/// wywołania wielokrotnie — istniejące wpisy nie są modyfikowane, żeby nie
/// nadpisać ustawień użytkownika.
pub async fn ensure_teams_bot_defaults(pool: &DbPool) -> Result<()> {
    for alias in TEAMS_ALIASES {
        ensure_alias(pool, alias)?;
    }
    ensure_teams_flow(pool)?;
    Ok(())
}

fn ensure_alias(pool: &DbPool, alias: &str) -> Result<()> {
    if repository::resolve_model_alias(pool, alias)?.is_some() {
        return Ok(());
    }
    // `target_model` zostaje pusty — zostanie uzupełniony ręcznie w UI.
    repository::create_model_alias(pool, alias, "", None, Some("first_available"))?;
    Ok(())
}

fn ensure_teams_flow(pool: &DbPool) -> Result<()> {
    if find_flow_by_name(pool, TEAMS_FLOW_NAME)?.is_some() {
        return Ok(());
    }

    let flow_json = json!({
        "nodes": [
            {
                "id": "t1",
                "type": "trigger",
                "position": { "x": 0, "y": 0 },
                "config": {}
            },
            {
                "id": "l1",
                "type": "llm",
                "position": { "x": 200, "y": 0 },
                "config": { "model_alias": "teams-summarization" }
            },
            {
                "id": "p1",
                "type": "pii_filter",
                "position": { "x": 400, "y": 0 },
                "config": {}
            },
            {
                "id": "o1",
                "type": "output",
                "position": { "x": 600, "y": 0 },
                "config": {}
            }
        ],
        "edges": [
            { "from": "t1", "to": "l1" },
            { "from": "l1", "to": "p1" },
            { "from": "p1", "to": "o1" }
        ]
    })
    .to_string();

    let params = FlowParams {
        name: TEAMS_FLOW_NAME,
        description: Some(
            "Domyslny flow dla teams-bot: trigger -> llm -> pii_filter -> output.",
        ),
        is_default: false,
        service_type: Some("agents"),
        flow_json: &flow_json,
        status: "active",
    };
    repository::create_flow(pool, &params)?;
    Ok(())
}

/// Skanuje pierwsze partie flow i zwraca ten o dopasowanej nazwie. Repository
/// nie udostępnia lookupu po nazwie, więc paginujemy ręcznie.
fn find_flow_by_name(pool: &DbPool, name: &str) -> Result<Option<crate::db::models::DbFlow>> {
    const PAGE: i64 = 100;
    let mut offset: i64 = 0;
    loop {
        let batch = repository::list_flows(pool, offset, PAGE)?;
        let batch_len = batch.len() as i64;
        if let Some(found) = batch.into_iter().find(|f| f.name == name) {
            return Ok(Some(found));
        }
        if batch_len < PAGE {
            return Ok(None);
        }
        offset += PAGE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn setup_pool() -> DbPool {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations::run(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[tokio::test]
    async fn ensure_teams_bot_defaults_creates_missing() {
        let pool = setup_pool();

        ensure_teams_bot_defaults(&pool).await.unwrap();

        for alias in TEAMS_ALIASES {
            let row = repository::resolve_model_alias(&pool, alias)
                .unwrap()
                .unwrap_or_else(|| panic!("alias {alias} not created"));
            assert_eq!(row.target_model, "");
            assert_eq!(row.strategy.as_deref(), Some("first_available"));
        }

        let flow = find_flow_by_name(&pool, TEAMS_FLOW_NAME)
            .unwrap()
            .expect("teams-flow should exist");
        assert_eq!(flow.service_type.as_deref(), Some("agents"));
        assert_eq!(flow.status, "active");
        assert_eq!(flow.is_default, false);

        // Parsowanie flow_json potwierdza że DAG jest poprawny strukturalnie.
        let parsed: serde_json::Value = serde_json::from_str(&flow.flow_json).unwrap();
        let nodes = parsed["nodes"].as_array().unwrap();
        let edges = parsed["edges"].as_array().unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(edges.len(), 3);

        // Kolejność węzłów i krawędzi: trigger -> llm -> pii_filter -> output.
        let node_types: Vec<&str> = nodes
            .iter()
            .map(|n| n["type"].as_str().unwrap())
            .collect();
        assert_eq!(node_types, vec!["trigger", "llm", "pii_filter", "output"]);

        // LLM musi mieć alias do routingu summaryzacji.
        let llm_node = nodes.iter().find(|n| n["type"] == "llm").unwrap();
        assert_eq!(llm_node["config"]["model_alias"], "teams-summarization");
    }

    #[tokio::test]
    async fn ensure_teams_bot_defaults_is_idempotent() {
        let pool = setup_pool();

        ensure_teams_bot_defaults(&pool).await.unwrap();
        ensure_teams_bot_defaults(&pool).await.unwrap();

        // Liczymy flows o nazwie teams-flow — musi być dokładnie 1.
        let flows = repository::list_flows(&pool, 0, 100).unwrap();
        let teams_flows = flows.iter().filter(|f| f.name == TEAMS_FLOW_NAME).count();
        assert_eq!(teams_flows, 1, "flow duplicated on second call");

        // Każdy alias pojawia się dokładnie raz (resolve zwraca is_active=1).
        for alias in TEAMS_ALIASES {
            let row = repository::resolve_model_alias(&pool, alias).unwrap();
            assert!(row.is_some(), "alias {alias} disappeared");
        }
    }

    #[tokio::test]
    async fn ensure_teams_bot_defaults_preserves_existing_aliases() {
        let pool = setup_pool();

        // User już ręcznie ustawił alias na konkretny model.
        repository::create_model_alias(
            &pool,
            "teams-summarization",
            "custom-model",
            None,
            Some("first_available"),
        )
        .unwrap();

        ensure_teams_bot_defaults(&pool).await.unwrap();

        let row = repository::resolve_model_alias(&pool, "teams-summarization")
            .unwrap()
            .unwrap();
        assert_eq!(
            row.target_model, "custom-model",
            "existing alias target_model was overwritten"
        );
    }
}
