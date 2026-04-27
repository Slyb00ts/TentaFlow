// =============================================================================
// Plik: meeting/native.rs
// Opis: Subprocess spawner dla teams-bota w trybie native (bez Dockera).
//       Uruchamia binarke `tentaflow-meeting` (zbudowana razem z `tentaflow`
//       przez build.rs) z env-em analogicznym do entry-pointu kontenera.
//       Trzyma uchwyt PID per session w globalnym rejestrze, na leave wysyla
//       SIGTERM (graceful — bot kliknie Leave w Teams w 1.5s) i czeka na exit.
// =============================================================================

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::port_pool::NativeAllocatedPorts;

/// Parametry startu subprocesu — analogiczne do `container::SpawnRequest`,
/// ale z `bridge_port` (Docker tego nie potrzebuje, ma stale 9999 w kontenerze).
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub session_id: i64,
    pub meeting_url: String,
    pub meeting_key: String,
    pub ports: NativeAllocatedPorts,
    pub secret_key_hex: String,
    pub bot_name: String,
    pub stt_alias: String,
    pub summarization_alias: String,
    pub tts_alias: String,
    pub flow_alias: String,
    pub llm_alias: String,
    pub respond_enabled: bool,
}

/// Wynik spawn — PID subprocesu (do logowania) + nazwa "kontenera" zgodna
/// z konwencja `meeting-bot-<session_id>` (uzywana w GUI / DB).
#[derive(Debug, Clone)]
pub struct SpawnOutcome {
    pub pid: u32,
    pub container_name: String,
}

/// Globalny rejestr aktywnych subprocessow per session_id. `Child` musi byc
/// "zywy" zeby SIGTERM doszedl, dlatego trzymamy go tutaj zamiast oddawac
/// callerowi (tak jak Docker container ID — ale tu nie ma stop API zewn.).
static REGISTRY: OnceLock<Mutex<HashMap<i64, Child>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<i64, Child>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Lokalizuje binarke `tentaflow-meeting` obok `tentaflow`. Zgodnie z konwencja
/// `tentaflow/build.rs` kopiuje ja do `target/<profile>/`. Po `cargo install`
/// laduje obok pliku binary tentaflow.
fn locate_meeting_binary() -> Result<std::path::PathBuf> {
    let bin_name = if cfg!(target_os = "windows") {
        "tentaflow-meeting.exe"
    } else {
        "tentaflow-meeting"
    };
    let exe = std::env::current_exe().context("std::env::current_exe()")?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("brak katalogu nadrzednego dla biezacej binarki"))?;
    let candidate = dir.join(bin_name);
    if candidate.is_file() {
        return Ok(candidate);
    }
    // Fallback: PATH (jesli user zainstalowal recznie)
    if let Some(found) = which_in_path(bin_name) {
        return Ok(found);
    }
    Err(anyhow!(
        "Nie znaleziono binarki {} obok tentaflow ani w PATH. \
         Zbuduj projekt 'cargo build --release' zeby `tentaflow/build.rs` \
         skompilowal sidecar.",
        bin_name
    ))
}

fn which_in_path(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Spawnuje sidecar bota jako subprocess. Env-y odpowiadaja kontraktowi
/// kontenera Docker (`MeetingConfig::from_env`). Stdout/stderr propagowane
/// do logow tentaflow.
pub async fn spawn(req: &SpawnRequest) -> Result<SpawnOutcome> {
    let bin = locate_meeting_binary()?;
    let name = container_name(req.session_id);

    // Bez `--config` — bot uzyje default `meeting.toml` ktory nie istnieje w
    // cwd subprocesu, wiec zalanduje w `from_env()`. Wszystkie potrzebne
    // wartosci podajemy jako env (kontrakt 1:1 z Dockerem, gdzie tez nie ma
    // pliku konfig — env-only).
    let mut cmd = Command::new(&bin);
    cmd.env("MEETING_URL", &req.meeting_url)
        .env("MEETING_ID", &req.meeting_key)
        .env("BOT_SECRET_KEY_HEX", &req.secret_key_hex)
        .env("BOT_NAME", &req.bot_name)
        .env("STT_ALIAS", &req.stt_alias)
        .env("SUMMARIZATION_ALIAS", &req.summarization_alias)
        .env("TTS_ALIAS", &req.tts_alias)
        .env("FLOW_ALIAS", &req.flow_alias)
        .env("LLM_ALIAS", &req.llm_alias)
        .env("RESPOND_ENABLED", if req.respond_enabled { "true" } else { "false" })
        .env("TRANSPORT_PORT", req.ports.quic.to_string())
        .env("TENTAFLOW_BRIDGE_PORT", req.ports.bridge.to_string())
        // Native = host network namespace, broadcast mDNS i DHT bylby widoczny
        // w mesh routera — wylaczamy zeby bot nie pojawil sie jako kandydat
        // do parowania.
        .env("ENABLE_LAN_DISCOVERY", "false")
        .env("ENABLE_DHT_DISCOVERY", "false")
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(false);

    let child = cmd
        .spawn()
        .with_context(|| format!("spawn subprocess {}", bin.display()))?;
    let pid = child.id().ok_or_else(|| anyhow!("subprocess nie ma PID"))?;

    info!(
        session = req.session_id,
        pid,
        bin = %bin.display(),
        "Meeting Bot subprocess spawned (native)"
    );

    let mut reg = registry().lock().await;
    reg.insert(req.session_id, child);
    Ok(SpawnOutcome {
        pid,
        container_name: name,
    })
}

/// Zatrzymuje subprocess: na unixie wysyla SIGTERM (bot ma 10s na clean exit
/// — kliknie Leave w Teams), potem fallback kill. Na Windows tylko kill, bo
/// SIGTERM nie ma odpowiednika (bot na Win nie zostal jeszcze wyteszowany
/// pod katem CtrlBreak).
pub async fn stop(session_id: i64) -> Result<()> {
    let mut reg = registry().lock().await;
    let Some(mut child) = reg.remove(&session_id) else {
        return Ok(());
    };

    let pid_opt = child.id();

    #[cfg(unix)]
    if let Some(pid) = pid_opt {
        // SIGTERM — bot ma signal handler ktory klika Leave w Teams.
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }

    #[cfg(not(unix))]
    let _ = pid_opt;

    // Czekaj max 10s na exit, potem kill.
    let exit = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait()).await;
    match exit {
        Ok(Ok(status)) => info!(session = session_id, ?status, "Meeting Bot subprocess zakonczony"),
        Ok(Err(e)) => warn!(session = session_id, "wait subprocess: {}", e),
        Err(_) => {
            warn!(session = session_id, "timeout 10s — wymuszam kill");
            let _ = child.kill().await;
        }
    }

    Ok(())
}

/// Sprzata wiszace subprocessy po unclean shutdown. Uzywane przy starcie
/// MeetingManagera. Iteruje przez aktywne sesje w DB i jesli ktoras nie
/// jest w naszym rejestrze, oznacza ja jako ended (proces juz nie istnieje
/// po restarcie tentaflow).
pub async fn cleanup_stale() -> Result<()> {
    // OnceLock + restart procesu = pusty rejestr, wiec nic do robienia tutaj.
    // MeetingManager::cleanup_on_startup zostawia DB w czystym stanie sam.
    Ok(())
}

/// Nazwa serwisu (taka sama konwencja jak Docker dla spojnosci log/dashboard).
pub fn container_name(session_id: i64) -> String {
    format!("meeting-bot-{}", session_id)
}
