// =============================================================================
// Plik: net/iroh/relay.rs
// Opis: Konfiguracja serwera relay iroh. Pobiera URL w kolejnosci:
//       1. DB `settings.mesh.iroh_relay_url` (ustawiany przez GUI admina)
//       2. config.toml `[mesh] iroh_relay_url`
//       3. domyslny publiczny relay n0 `use.iroh.network`.
//       Walidacja: URL musi byc prawidlowym HTTPS schema, inaczej zwraca
//       default z log warning.
// =============================================================================

use crate::config::MeshConfig;
use crate::db::{self, DbPool};
use iroh::RelayUrl;

/// Klucz w tabeli `settings` dla URL serwera relay iroh.
pub const RELAY_URL_SETTING_KEY: &str = "mesh.iroh_relay_url";

/// Publiczny serwer relay hostowany przez n0 — stosowany gdy nic innego
/// nie jest skonfigurowane.
pub const DEFAULT_RELAY_URL: &str = "https://use.iroh.network./";

/// Zwraca `RelayUrl` na podstawie priorytetu DB → config → default.
/// Niepoprawny URL w DB lub configu jest zastepowany domyslnym z warn logiem.
pub fn load_relay_url(db: &DbPool, mesh_cfg: Option<&MeshConfig>) -> RelayUrl {
    if let Ok(Some(raw)) = db::repository::get_setting(db, RELAY_URL_SETTING_KEY) {
        if let Ok(url) = parse_relay_url(&raw) {
            return url;
        }
        tracing::warn!(
            raw = %raw,
            "Invalid iroh relay URL w DB settings — fallback do config/default"
        );
    }

    if let Some(cfg) = mesh_cfg {
        if !cfg.iroh_relay_url.is_empty() {
            if let Ok(url) = parse_relay_url(&cfg.iroh_relay_url) {
                return url;
            }
            tracing::warn!(
                raw = %cfg.iroh_relay_url,
                "Invalid iroh relay URL w config.toml — fallback do default"
            );
        }
    }

    parse_relay_url(DEFAULT_RELAY_URL)
        .expect("domyslny relay URL zawsze sie parsuje")
}

fn parse_relay_url(raw: &str) -> Result<RelayUrl, url::ParseError> {
    let trimmed = raw.trim();
    let url: url::Url = trimmed.parse()?;
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_url_parses() {
        let url = parse_relay_url(DEFAULT_RELAY_URL).unwrap();
        assert_eq!(url.scheme(), "https");
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
}
