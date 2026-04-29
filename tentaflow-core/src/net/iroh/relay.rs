// =============================================================================
// Plik: net/iroh/relay.rs
// Opis: Rozwiazywanie URL relay iroh na podstawie priorytetu:
//       1. DB `settings.mesh.iroh_relay_url` (ustawiany przez GUI admina),
//       2. config.toml `[mesh] iroh_relay_url`,
//       3. `None` — oznacza uzycie wbudowanego presetu N0 iroh (4 regiony
//          `*.relay.n0.iroh-canary.iroh.link`), co pozwala `endpoint.rs`
//          zostawic `RelayMode::Default`. Walidacja wymaga schematu
//          `http`/`https`; niepoprawne wartosci sa logowane i pomijane.
// =============================================================================

use crate::config::MeshConfig;
use crate::db::{self, DbPool};
use iroh::RelayUrl;

/// Klucz w tabeli `settings` dla URL serwera relay iroh.
pub const RELAY_URL_SETTING_KEY: &str = "mesh.iroh_relay_url";

/// Domyslny relay TentaFlow uzywany gdy DB ani config.toml nie maja
/// wlasnego URL. Self-hosted iroh-relay pod nextapp.pl, dostepny po
/// HTTPS. Zastepuje pre-iroh-0.98 default `use.iroh.network` ktory padl.
pub const DEFAULT_RELAY_URL: &str = "https://relay.nextapp.pl";

/// Zwraca `Some(RelayUrl)` gdy admin skonfigurowal custom relay (DB lub
/// config.toml), albo `None` gdy nic nie ustawiono — wtedy iroh uzywa
/// wbudowanego presetu N0 z 4 produkcyjnymi relayami. Niepoprawny URL
/// w DB degraduje do config, niepoprawny URL w config degraduje do `None`.
pub fn load_relay_url(db: Option<&DbPool>, mesh_cfg: Option<&MeshConfig>) -> Option<RelayUrl> {
    if let Some(pool) = db {
        match db::repository::get_setting(pool, RELAY_URL_SETTING_KEY) {
            Ok(Some(raw)) => match parse_relay_url(&raw) {
                Ok(url) => return Some(url),
                Err(err) => tracing::warn!(
                    raw = %raw,
                    error = %err,
                    "Nieprawidlowy iroh relay URL w DB settings — fallback do config"
                ),
            },
            Ok(None) => {}
            Err(err) => tracing::warn!(
                error = %err,
                "Nie udalo sie odczytac ustawienia relay iroh z DB — fallback do config"
            ),
        }
    }

    if let Some(cfg) = mesh_cfg {
        let trimmed = cfg.iroh_relay_url.trim();
        if !trimmed.is_empty() {
            match parse_relay_url(trimmed) {
                Ok(url) => return Some(url),
                Err(err) => tracing::warn!(
                    raw = %trimmed,
                    error = %err,
                    "Nieprawidlowy iroh relay URL w config.toml — uzywam DEFAULT_RELAY_URL"
                ),
            }
        }
    }

    parse_relay_url(DEFAULT_RELAY_URL).ok()
}

/// Parsuje `raw` jako URL i wymusza scheme `http`/`https`. Inne schematy
/// (np. `ftp`, `ws`) sa odrzucane, bo iroh relay komunikuje sie po HTTP(S).
fn parse_relay_url(raw: &str) -> Result<RelayUrl, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("URL jest pusty".to_string());
    }
    let url: url::Url = trimmed
        .parse()
        .map_err(|e: url::ParseError| format!("blad parsowania: {e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url.into()),
        other => Err(format!(
            "nieobslugiwany scheme `{other}` (wymagany http/https)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_relay_url_accepts_https() {
        let url = parse_relay_url("https://relay.example./").unwrap();
        assert_eq!(url.scheme(), "https");
    }

    #[test]
    fn parse_relay_url_accepts_http() {
        let url = parse_relay_url("http://relay.example./").unwrap();
        assert_eq!(url.scheme(), "http");
    }

    #[test]
    fn parse_relay_url_rejects_bad_scheme() {
        let err = parse_relay_url("ftp://relay.example./").unwrap_err();
        assert!(err.contains("ftp"));
    }

    #[test]
    fn puste_trimowane() {
        let url = parse_relay_url("   https://relay.example./   ").unwrap();
        assert_eq!(url.host_str(), Some("relay.example."));
    }

    #[test]
    fn niepoprawny_url_zwraca_blad() {
        assert!(parse_relay_url("nie-jest-url").is_err());
    }

    #[test]
    fn pusty_string_zwraca_blad() {
        assert!(parse_relay_url("   ").is_err());
    }
}
