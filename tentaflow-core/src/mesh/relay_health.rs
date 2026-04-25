// =============================================================================
// Plik: mesh/relay_health.rs
// Opis: Background task pingujacy serwer relay iroh co 30s i utrzymujacy
//       wspoldzielony stan (`Arc<RwLock<RelayHealth>>`) dla GUI. Status liczony
//       z ostatnich 3 wynikow + RTT: connected / degraded / unreachable.
//       Gdy `relay_url == None`, monitor wpisuje `disabled` i nie pinguje.
// =============================================================================

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::RelayUrl;
use parking_lot::RwLock;
use tokio_util::sync::CancellationToken;

/// Snapshot stanu relay widziany przez GUI. Aktualizowany co 30s przez
/// `spawn_relay_health_monitor`. `bind_addr_actual` jest wstrzykiwany przy
/// starcie i nie zmienia sie az do restartu mesh pipeline.
#[derive(Clone, Debug)]
pub struct RelayHealth {
    pub url: String,
    pub reachable: bool,
    pub rtt_ms: Option<u32>,
    pub last_check_unix_secs: i64,
    pub last_success_unix_secs: Option<i64>,
    pub status: String,
    pub bind_addr_actual: String,
}

impl RelayHealth {
    /// Stan poczatkowy gdy relay jest wlaczony — jeszcze nie pingowany.
    pub fn initial_pending(url: String, bind_addr_actual: String) -> Self {
        Self {
            url,
            reachable: false,
            rtt_ms: None,
            last_check_unix_secs: 0,
            last_success_unix_secs: None,
            status: "unreachable".to_string(),
            bind_addr_actual,
        }
    }

    /// Stan gdy nie skonfigurowano custom relay (uzywany jest preset N0 albo
    /// brak relay). GUI dostaje pusty URL i status `disabled`.
    pub fn disabled(bind_addr_actual: String) -> Self {
        Self {
            url: String::new(),
            reachable: false,
            rtt_ms: None,
            last_check_unix_secs: 0,
            last_success_unix_secs: None,
            status: "disabled".to_string(),
            bind_addr_actual,
        }
    }
}

/// Interwal pingow + timeout pojedynczej proby. 5s timeout zostawia margines
/// dla wolnych laczy bez blokowania interwalu pingow.
const PING_INTERVAL: Duration = Duration::from_secs(30);
const PING_TIMEOUT: Duration = Duration::from_secs(5);

/// Prog "degraded" — gdy RTT przekracza 200ms ale relay odpowiada, GUI pokaze
/// pomarancz zamiast zielonej kropki.
const DEGRADED_RTT_MS: u32 = 200;

/// Iroh-relay reaguje GET'em na root path roznymi statusami w zaleznosci od
/// wersji (200/204/426/upgrade-required). Wystarczy ze TCP+TLS handshake +
/// HTTP response sie odbedzie — wszystko inne traktujemy jako "live".
fn probe_url(base: &str) -> String {
    base.trim_end_matches('/').to_string() + "/"
}

/// Spawnuje tokio task ktory co `PING_INTERVAL` pinguje relay i aktualizuje
/// `state`. Cancellation token zatrzymuje petle czysto przy shutdownie. Gdy
/// `relay_url == None`, task ustawia `disabled` raz i konczy sie — nie ma co
/// pingowac.
pub fn spawn_relay_health_monitor(
    relay_url: Option<RelayUrl>,
    bind_addr_actual: String,
    state: Arc<RwLock<RelayHealth>>,
    shutdown: CancellationToken,
) {
    let Some(url) = relay_url else {
        *state.write() = RelayHealth::disabled(bind_addr_actual);
        tracing::info!("relay health monitor: disabled (brak skonfigurowanego relay)");
        return;
    };

    let url_str = url.to_string();
    *state.write() = RelayHealth::initial_pending(url_str.clone(), bind_addr_actual.clone());

    let probe_target = probe_url(&url_str);
    let client = match reqwest::Client::builder()
        .timeout(PING_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(error = %err, "relay health monitor: nie udalo sie zbudowac reqwest client");
            return;
        }
    };

    tokio::spawn(async move {
        // Slidng window 3 ostatnich wynikow (true=ok, false=fail) do kalkulacji
        // statusu degraded/unreachable. 3 z rzedu fail = unreachable.
        let mut window: VecDeque<bool> = VecDeque::with_capacity(3);
        let mut last_status = String::new();

        loop {
            let started = Instant::now();
            let now_secs = chrono::Utc::now().timestamp();
            let result = client.get(&probe_target).send().await;
            let rtt_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
            let success = matches!(&result, Ok(_));

            window.push_back(success);
            while window.len() > 3 {
                window.pop_front();
            }

            let consecutive_fails = window.iter().rev().take_while(|ok| !**ok).count();
            let any_fail_in_window = window.iter().any(|ok| !ok);

            let new_status = if !success && consecutive_fails >= 3 {
                "unreachable"
            } else if !success {
                "degraded"
            } else if any_fail_in_window || rtt_ms > DEGRADED_RTT_MS {
                "degraded"
            } else {
                "connected"
            };

            {
                let mut guard = state.write();
                guard.last_check_unix_secs = now_secs;
                guard.status = new_status.to_string();
                if success {
                    guard.reachable = true;
                    guard.rtt_ms = Some(rtt_ms);
                    guard.last_success_unix_secs = Some(now_secs);
                } else {
                    guard.reachable = false;
                    guard.rtt_ms = None;
                }
            }

            if last_status != new_status {
                match &result {
                    Ok(resp) => tracing::info!(
                        status = new_status,
                        rtt_ms,
                        http = resp.status().as_u16(),
                        url = %probe_target,
                        "relay health check"
                    ),
                    Err(err) => tracing::info!(
                        status = new_status,
                        error = %err,
                        url = %probe_target,
                        "relay health check"
                    ),
                }
                last_status = new_status.to_string();
            }

            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::debug!("relay health monitor: shutdown");
                    return;
                }
                _ = tokio::time::sleep(PING_INTERVAL) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_state_has_empty_url_and_disabled_status() {
        let h = RelayHealth::disabled("0.0.0.0:8090".to_string());
        assert_eq!(h.url, "");
        assert_eq!(h.status, "disabled");
        assert!(!h.reachable);
        assert!(h.rtt_ms.is_none());
        assert_eq!(h.bind_addr_actual, "0.0.0.0:8090");
    }

    #[test]
    fn initial_pending_carries_url_and_unreachable_status() {
        let h = RelayHealth::initial_pending(
            "https://relay.example/".to_string(),
            "192.168.1.10:8090".to_string(),
        );
        assert_eq!(h.url, "https://relay.example/");
        assert_eq!(h.status, "unreachable");
        assert_eq!(h.bind_addr_actual, "192.168.1.10:8090");
    }

    #[test]
    fn probe_url_normalizes_trailing_slash() {
        assert_eq!(probe_url("https://relay.example"), "https://relay.example/");
        assert_eq!(probe_url("https://relay.example/"), "https://relay.example/");
        assert_eq!(probe_url("https://relay.example///"), "https://relay.example/");
    }
}
