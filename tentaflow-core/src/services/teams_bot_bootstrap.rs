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

/// Domyslne wake-words dodawane przy pierwszym deploy teams-bota.
/// Edytowalne przez UI/API; po edycji tabela jest "user-managed" — nie
/// nadpisujemy. Ten seed dotyka tylko pustej tabeli.
const DEFAULT_WAKE_WORDS: &[&str] = &["jarvis", "tentaflow", "asystencie", "asystent", "bot"];

/// Tworzy (jeśli brak) domyślne aliasy dla teams-bota. Bezpieczna do
/// wywołania wielokrotnie — istniejące wpisy nie są modyfikowane, żeby nie
/// nadpisać ustawień użytkownika. Flow orchestratora nie jest seedowany —
/// default to "Default Chat", a konkretny flow przypisuje user w ustawieniach
/// Meeting Bota (`flow_alias`).
pub async fn ensure_teams_bot_defaults(pool: &DbPool) -> Result<()> {
    for alias in TEAMS_ALIASES {
        ensure_alias(pool, alias)?;
    }
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
    }

    #[tokio::test]
    async fn ensure_teams_bot_defaults_is_idempotent() {
        let pool = setup_pool();

        ensure_teams_bot_defaults(&pool).await.unwrap();
        ensure_teams_bot_defaults(&pool).await.unwrap();

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
