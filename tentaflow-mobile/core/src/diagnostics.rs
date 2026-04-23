// =============================================================================
// Plik: diagnostics.rs
// Opis: Diagnostyka cyklu zycia mobile — odroznia cold start po suspendzie
//       (iOS wznowil nasz proces) od cold starta po terminate (iOS zabil
//       apke i zrobiono relaunch). Heartbeat zapisuje timestamp co 5s do
//       last_alive.txt; przy starcie porownujemy czas plik-vs-teraz.
// =============================================================================

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::watch;
use tracing::{info, warn};

const HEARTBEAT_FILENAME: &str = "last_alive.txt";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// Prog w sekundach, ponizej ktorego uznajemy start za wake po suspendzie.
/// iOS typowo suspenduje proces na minuty/dziesiatki minut — powyzej 30 min
/// praktycznie zawsze idzie terminate.
const SUSPEND_THRESHOLD_SECS: i64 = 30 * 60;

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn heartbeat_path(data_dir: &Path) -> PathBuf {
    data_dir.join(HEARTBEAT_FILENAME)
}

fn read_last_alive(data_dir: &Path) -> Option<i64> {
    fs::read_to_string(heartbeat_path(data_dir))
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
}

/// Wolane raz przy starcie runtime. Loguje czy to byl wake po suspendzie
/// czy pelny cold start (po terminate / pierwsze uruchomienie).
pub fn diagnose_startup(data_dir: &Path) {
    match read_last_alive(data_dir) {
        Some(last) => {
            let diff = now_secs() - last;
            if diff < 0 {
                warn!("[diag] zegar cofniety? last_alive={} now={} diff={}", last, now_secs(), diff);
            } else if diff < SUSPEND_THRESHOLD_SECS {
                info!("[diag] startup type=resume_after_suspend delta_s={}", diff);
            } else {
                info!("[diag] startup type=cold_start_after_terminate delta_min={}", diff / 60);
            }
        }
        None => {
            info!("[diag] startup type=first_launch (brak last_alive.txt)");
        }
    }
}

/// Spawnuje tokio task ktory zapisuje aktualny timestamp co 5 sekund.
/// Konczy sie gdy shutdown broadcast wysle true.
pub fn spawn_heartbeat_task(data_dir: PathBuf, mut shutdown: watch::Receiver<bool>) {
    tokio::spawn(async move {
        let path = heartbeat_path(&data_dir);
        // Pierwszy zapis od razu — zeby kolejny cold start po szybkim
        // terminate (np. crash w pierwszej sekundzie) tez sie policzyl.
        let _ = fs::write(&path, now_secs().to_string());

        let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
        interval.tick().await; // pierwszy tick jest natychmiastowy — konsumuj

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let _ = fs::write(&path, now_secs().to_string());
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    });
}
