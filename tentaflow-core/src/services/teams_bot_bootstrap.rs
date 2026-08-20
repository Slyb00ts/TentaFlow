// =============================================================================
// Plik: teams_bot_bootstrap.rs
// Opis: Idempotentna inicjalizacja domyślnych aliasów modeli dla teams-bota.
// =============================================================================

use anyhow::Result;

use crate::db::{repository, DbPool};

/// Nazwy aliasów używanych przez teams-bota — pusty `target_model` sygnalizuje
/// że user powinien przypisać konkretny model w UI. `teams-llm` jest LLM
/// generujacy odpowiedzi bota w real-time, oddzielny od `teams-summarization`
/// ktory robi okresowe podsumowania.
const TEAMS_ALIASES: &[&str] = &["teams-stt", "teams-summarization", "teams-tts", "teams-llm"];

/// Tworzy (jeśli brak) domyślne aliasy dla teams-bota. Bezpieczna do
/// wywołania wielokrotnie — istniejące wpisy nie są modyfikowane, żeby nie
/// nadpisać ustawień użytkownika. Flow orchestratora nie jest seedowany —
/// default to "Default Chat", a konkretny flow przypisuje user w ustawieniach
/// Meeting Bota (`flow_id`).
pub async fn ensure_teams_bot_defaults(pool: &DbPool) -> Result<()> {
    for alias in TEAMS_ALIASES {
        ensure_alias(pool, alias)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn setup_pool() -> DbPool {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations::run(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    // Seedowany alias jest "parked" (pusty `target_model` → `is_active = 0`),
    // więc `resolve_model_alias` go nie zwraca — tylko `list_model_aliases`
    // pokazuje również wpisy nieaktywne.
    fn find_alias(pool: &DbPool, alias: &str) -> Option<crate::db::models::DbModelAlias> {
        repository::list_model_aliases(pool)
            .unwrap()
            .into_iter()
            .find(|row| row.alias == alias)
    }

    #[tokio::test]
    async fn ensure_teams_bot_defaults_creates_missing() {
        let pool = setup_pool();

        ensure_teams_bot_defaults(&pool).await.unwrap();

        for alias in TEAMS_ALIASES {
            let row =
                find_alias(&pool, alias).unwrap_or_else(|| panic!("alias {alias} not created"));
            assert_eq!(row.target_model, "");
            assert_eq!(row.strategy.as_deref(), Some("first_available"));
            assert!(
                !row.is_active,
                "alias {alias} bez target_model musi zostać parked (is_active = 0)"
            );
        }
    }

    #[tokio::test]
    async fn ensure_teams_bot_defaults_is_idempotent() {
        let pool = setup_pool();

        ensure_teams_bot_defaults(&pool).await.unwrap();
        ensure_teams_bot_defaults(&pool).await.unwrap();

        // Każdy alias pojawia się dokładnie raz i pozostaje parked
        // (is_active = 0) — powtórny seed nie może go "obudzić".
        for alias in TEAMS_ALIASES {
            let rows = repository::list_model_aliases(&pool).unwrap();
            let matching: Vec<_> = rows.iter().filter(|row| row.alias == *alias).collect();
            assert_eq!(matching.len(), 1, "alias {alias} nie występuje dokładnie raz");
            assert_eq!(matching[0].target_model, "");
            assert_eq!(matching[0].strategy.as_deref(), Some("first_available"));
            assert!(
                !matching[0].is_active,
                "powtórny seed nie może aktywować aliasu {alias} bez target_model"
            );
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
