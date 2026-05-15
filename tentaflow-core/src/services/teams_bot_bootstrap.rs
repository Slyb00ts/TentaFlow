// =============================================================================
// Plik: teams_bot_bootstrap.rs
// Opis: Idempotentna inicjalizacja domyślnych aliasów modeli i flow dla teams-bota.
// =============================================================================

use anyhow::Result;
use serde_json::json;

use crate::db::models::FlowParams;
use crate::db::{repository, DbPool};

/// Nazwy aliasów używanych przez teams-bota — pusty `target_model` sygnalizuje
/// że user powinien przypisać konkretny model w UI. `teams-llm` jest LLM
/// generujacy odpowiedzi bota w real-time, oddzielny od `teams-summarization`
/// ktory robi okresowe podsumowania.
const TEAMS_ALIASES: &[&str] = &["teams-stt", "teams-summarization", "teams-tts", "teams-llm"];

/// Nazwa domyślnego flow dla teams-bota.
const TEAMS_FLOW_NAME: &str = "teams-flow";

/// Domyslne wake-words dodawane przy pierwszym deploy teams-bota.
/// Edytowalne przez UI/API; po edycji tabela jest "user-managed" — nie
/// nadpisujemy. Ten seed dotyka tylko pustej tabeli.
const DEFAULT_WAKE_WORDS: &[&str] = &["jarvis", "tentaflow", "asystencie", "asystent", "bot"];

/// Tworzy (jeśli brak) domyślne aliasy i flow dla teams-bota. Bezpieczna do
/// wywołania wielokrotnie — istniejące wpisy nie są modyfikowane, żeby nie
/// nadpisać ustawień użytkownika.
pub async fn ensure_teams_bot_defaults(pool: &DbPool) -> Result<()> {
    for alias in TEAMS_ALIASES {
        ensure_alias(pool, alias)?;
    }
    ensure_teams_flow(pool)?;
    ensure_default_wake_words(pool)?;
    Ok(())
}

/// Idempotentnie seeduje domyslne wake-words gdy tabela jest pusta. Po
/// pierwszej edycji uzytkownika (dodanie/usuniecie) zostawiamy w spokoju.
fn ensure_default_wake_words(pool: &DbPool) -> Result<()> {
    let existing = repository::list_wake_words(pool)?;
    if !existing.is_empty() {
        return Ok(());
    }
    for w in DEFAULT_WAKE_WORDS {
        let _ = repository::add_wake_word(pool, w);
    }
    Ok(())
}

fn ensure_alias(pool: &DbPool, alias: &str) -> Result<()> {
    // R7.P2: musimy obsluzyc rowniez wpisy *nieaktywne* — `resolve_model_alias`
    // zwraca tylko aktywne, wiec wczesniejszy `is_some()` + INSERT walil sie
    // o `alias TEXT UNIQUE` przy reaktywacji bota po deaktywacji.
    // `create_or_reactivate_model_alias` robi atomicznie: jak istnieje (active
    // lub inactive) → reactivate (z chain-checkiem); inaczej → INSERT.
    // `target_model` zostaje pusty — zostanie uzupelniony recznie w UI.
    repository::create_or_reactivate_model_alias(
        pool,
        alias,
        "",
        "first_available",
        "addon",
        Some("teams-bot"),
    )?;
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
        description: Some("Domyslny flow dla teams-bot: trigger -> llm -> pii_filter -> output."),
        is_default: false,
        service_type: Some("agents"),
        flow_json: &flow_json,
        status: "active",
        published_model_name: None,
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
            let row = repository::resolve_model_alias(&pool, alias, None)
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
        let node_types: Vec<&str> = nodes.iter().map(|n| n["type"].as_str().unwrap()).collect();
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
            let row = repository::resolve_model_alias(&pool, alias, None).unwrap();
            assert!(row.is_some(), "alias {alias} disappeared");
        }
    }

    #[tokio::test]
    async fn ensure_teams_bot_defaults_preserves_existing_aliases() {
        let pool = setup_pool();

        // User już ręcznie ustawił alias na konkretny model.
        repository::create_model_alias_with_chain_check(
            &pool,
            "teams-summarization",
            "custom-model",
            None,
            Some("first_available"),
        )
        .unwrap();

        ensure_teams_bot_defaults(&pool).await.unwrap();

        let row = repository::resolve_model_alias(&pool, "teams-summarization", None)
            .unwrap()
            .unwrap();
        assert_eq!(
            row.target_model, "custom-model",
            "existing alias target_model was overwritten"
        );
    }
}
