// =============================================================================
// Plik: meeting/manager.rs
// Opis: Wysokopoziomowa orkiestracja sesji Meeting Bot. `MeetingManager` jest
//       współdzielony przez handlery protokołu. Ekspozycja: start_session,
//       leave_session, session_detail, session_list, generate_summary.
// =============================================================================

use anyhow::{anyhow, Context, Result};
use iroh::SecretKey;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

use crate::db::{repository, DbPool};
use crate::routing::service_manager::ServiceManager;

use super::container::{self, SpawnRequest};
use super::port_pool;

#[derive(Debug, Clone)]
pub struct StartSessionRequest {
    pub meeting_url: String,
    pub title: Option<String>,
    pub platform: String,
    pub owner_user_id: Option<i64>,
    pub bot_name: String,
    pub stt_alias: Option<String>,
    pub summarization_alias: Option<String>,
    pub tts_alias: Option<String>,
    pub flow_alias: Option<String>,
}

/// Domyślne aliasy przekazywane do kontenera teams-bota, jeśli caller nie
/// nadpisze. Zgodne z tymi zainicjalizowanymi przez `ensure_teams_bot_defaults`
/// w batch T1.5 oraz z `config.rs` bota (odczytuje env `*_ALIAS`).
pub const DEFAULT_STT_ALIAS: &str = "teams-stt";
pub const DEFAULT_SUMMARIZATION_ALIAS: &str = "teams-summarization";
pub const DEFAULT_TTS_ALIAS: &str = "teams-tts";
pub const DEFAULT_FLOW_ALIAS: &str = "teams-flow";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDescriptor {
    pub session_id: i64,
    pub meeting_key: String,
    pub meeting_url: Option<String>,
    pub title: Option<String>,
    pub status: String,
    pub started_at: String,
    pub last_activity_at: String,
    pub ended_at: Option<String>,
    pub platform: Option<String>,
    pub entry_count: i64,
    pub quic_port: Option<u16>,
    pub vnc_port: Option<u16>,
    pub novnc_port: Option<u16>,
    pub bot_endpoint_id: Option<String>,
    pub container_name: Option<String>,
    pub owner_user_id: Option<i64>,
    /// Aliasy przekazane do kontenera w env vars (efektywne — po zastosowaniu
    /// defaultów). Widoczne w dashboardzie żeby user wiedział co bot używa.
    pub stt_alias: String,
    pub summarization_alias: String,
    pub tts_alias: String,
    pub flow_alias: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: i64,
    pub tldr: String,
    pub decisions: String,
    pub action_items_json: String,
    pub open_questions: String,
    pub model: String,
    pub generated_at: String,
}

#[derive(Clone)]
pub struct MeetingManager {
    db: DbPool,
    /// Opcjonalny — cleanup path (startup) nie potrzebuje ServiceManagera.
    /// Production start_session wywołany przez handler zawsze dostaje Some.
    service_manager: Option<Arc<ServiceManager>>,
}

impl MeetingManager {
    pub fn new(db: DbPool, service_manager: Option<Arc<ServiceManager>>) -> Arc<Self> {
        Arc::new(Self {
            db,
            service_manager,
        })
    }

    /// Nazwa serwisu używana przy register_quic_service — zawiera session_id dla
    /// unikalności. Substring "meeting-bot" wymuszany przez spawn_connection_tasks
    /// żeby trafić do dedykowanego `meeting_bot_connection_loop` (reverse listener
    /// + transcript subscriber).
    fn service_name(session_id: i64) -> String {
        format!("meeting-bot-{}", session_id)
    }

    /// Startuje nową sesję Meeting Bot. Flow:
    /// 1. INSERT meeting_sessions (status=idle)
    /// 2. Alokuj trójkę portów (quic/vnc/novnc)
    /// 3. Wygeneruj Ed25519 secret key bota, oblicz endpoint_id (public key hex)
    /// 4. Zaktualizuj sesję (status=joining, container info, ports, keys)
    /// 5. Spawn kontener z env
    /// Jeśli którykolwiek krok zawiedzie — cofnij i zwroc blad.
    pub async fn start_session(&self, req: StartSessionRequest) -> Result<SessionDescriptor> {
        // User mógł usunąć alias w DB albo system jest świeżo po upgrade bazy —
        // idempotentnie przywracamy domyślne aliasy i flow przed spawnem. Best-effort,
        // błąd nie blokuje startu (bot może i tak działać z częściową konfiguracją).
        if let Err(e) =
            crate::services::teams_bot_bootstrap::ensure_teams_bot_defaults(&self.db).await
        {
            warn!("ensure_teams_bot_defaults przy start_session: {}", e);
        }

        // meeting_key — UUID zapewnia unikalność, przekazywany przez env MEETING_ID
        // do kontenera. Bot umieszcza ten sam string w `meeting_id` każdego STT
        // responsu, przez co router zapisuje transkrypty pod tą samą sesją
        // (meeting_sessions.meeting_key) którą tu tworzymy. Jedna sesja w DB, nie
        // dwie jak przed naprawą.
        let meeting_key = format!("mtg-{}", uuid::Uuid::new_v4());

        let session_id = repository::transcripts::get_or_create_session(
            &self.db,
            &meeting_key,
            Some(&req.meeting_url),
            req.title.as_deref(),
        )
        .context("create meeting_sessions row")?;

        // Przypisz owner_user_id od razu (get_or_create nie przyjmuje go).
        if let Some(uid) = req.owner_user_id {
            let conn = self.db.lock().unwrap();
            let _ = conn.execute(
                "UPDATE meeting_sessions SET owner_user_id = ?2 WHERE id = ?1",
                rusqlite::params![session_id, uid],
            );
            drop(conn);
        }

        let ports = match port_pool::allocate_for_session(&self.db, session_id) {
            Ok(p) => p,
            Err(e) => {
                let _ = repository::transcripts::mark_session_ended(&self.db, session_id);
                return Err(anyhow!("alokacja portow: {e}"));
            }
        };

        // Generuj secret key bota — iroh::SecretKey ma endpoint_id (public key) wbudowany.
        let secret = SecretKey::generate();
        let secret_key_hex = hex::encode(secret.to_bytes());
        let bot_endpoint_id = hex::encode(secret.public().as_bytes());

        repository::transcripts::update_session_spawned(
            &self.db,
            session_id,
            "",
            &container::container_name(session_id),
            ports.quic,
            ports.vnc,
            ports.novnc,
            &bot_endpoint_id,
            &secret_key_hex,
            &req.platform,
            req.owner_user_id,
        )?;

        // Efektywne aliasy — nadpisanie od callera lub domyślne z T1.5.
        let (stt_alias, summarization_alias, tts_alias, flow_alias) = resolve_aliases(&req);

        let spawn_req = SpawnRequest {
            session_id,
            meeting_url: req.meeting_url.clone(),
            meeting_key: meeting_key.clone(),
            ports,
            secret_key_hex: secret_key_hex.clone(),
            bot_name: req.bot_name.clone(),
            stt_alias: stt_alias.clone(),
            summarization_alias: summarization_alias.clone(),
            tts_alias: tts_alias.clone(),
            flow_alias: flow_alias.clone(),
        };

        match container::spawn(&spawn_req).await {
            Ok(outcome) => {
                // Uaktualnij rzeczywisty container_id zwrocony przez docker.
                let conn = self.db.lock().unwrap();
                let _ = conn.execute(
                    "UPDATE meeting_sessions SET container_id = ?2 WHERE id = ?1",
                    rusqlite::params![session_id, outcome.container_id],
                );
                drop(conn);
                info!(
                    session = session_id,
                    bot_endpoint_id = %bot_endpoint_id,
                    "Meeting session spawnowana"
                );
            }
            Err(e) => {
                // Rollback — zwolnij porty, oznacz ended.
                warn!("Spawn kontenera nieudany: {}", e);
                let _ = port_pool::release_for_session(&self.db, session_id);
                let _ = repository::transcripts::mark_session_ended(&self.db, session_id);
                return Err(anyhow!("spawn kontenera: {e}"));
            }
        }

        // Rejestracja w ServiceManager — router spawnuje meeting_bot_connection_loop
        // który łączy się do bota przez iroh. Direct addr 127.0.0.1:<quic_port>
        // omija LAN discovery (bridge network nie widoczna przez mDNS hosta).
        if let Some(ref sm) = self.service_manager {
            let service_name = Self::service_name(session_id);
            let iroh_url = format!("iroh://{}", bot_endpoint_id);
            let direct_addrs = vec![format!("127.0.0.1:{}", ports.quic)];
            sm.register_quic_service_with_addrs(
                service_name,
                "meeting-bot",
                iroh_url,
                None,
                None,
                direct_addrs,
            );
        } else {
            warn!(
                session = session_id,
                "brak ServiceManager — kontener uruchomiony ale router nie połączy się"
            );
        }

        // Deskryptor budowany z DB + efektywnych aliasów (te nie są persystowane
        // w meeting_sessions — pochodzą wyłącznie z env kontenera). Dzięki temu
        // odpowiedź start_session zawiera wartości faktycznie przekazane botowi.
        let mut desc = self
            .session_detail(session_id)?
            .ok_or_else(|| anyhow!("nie udalo sie pobrac sesji po spawnie"))?;
        desc.stt_alias = stt_alias;
        desc.summarization_alias = summarization_alias;
        desc.tts_alias = tts_alias;
        desc.flow_alias = flow_alias;
        Ok(desc)
    }

    /// Zatrzymuje sesję: wyrejestruj z ServiceManager → stop+rm kontener →
    /// release portów → status=ended. Kolejność: najpierw odłącz router
    /// (żeby nie walił reconnect loopem w umierający kontener), potem stop.
    pub async fn leave_session(&self, session_id: i64) -> Result<()> {
        repository::transcripts::set_session_status(&self.db, session_id, "leaving")?;
        if let Some(ref sm) = self.service_manager {
            sm.remove_quic_service(&Self::service_name(session_id), "meeting-bot");
        }
        let _ = container::stop(session_id).await;
        port_pool::release_for_session(&self.db, session_id)?;
        repository::transcripts::mark_session_ended(&self.db, session_id)?;
        info!(session = session_id, "Meeting session zakonczona");
        Ok(())
    }

    /// Sprząta sesje które zostały jako "active" po unclean shutdown.
    pub async fn cleanup_on_startup(&self) -> Result<()> {
        let _ = container::cleanup_stale_containers().await;
        let stale = repository::transcripts::list_stale_sessions(&self.db)?;
        for row in stale {
            warn!(
                session = row.id,
                status = %row.status,
                "stale meeting session po poprzednim starcie — oznaczam jako ended"
            );
            let _ = port_pool::release_for_session(&self.db, row.id);
            let _ = repository::transcripts::mark_session_ended(&self.db, row.id);
        }
        Ok(())
    }

    pub fn session_detail(&self, session_id: i64) -> Result<Option<SessionDescriptor>> {
        let row = match repository::transcripts::get_session(&self.db, session_id)? {
            Some(r) => r,
            None => return Ok(None),
        };
        Ok(Some(row_to_descriptor(&row)))
    }

    pub fn session_list(&self, owner_user_id: Option<i64>) -> Result<Vec<SessionDescriptor>> {
        let rows = repository::transcripts::list_sessions(&self.db, owner_user_id)?;
        Ok(rows.iter().map(row_to_descriptor).collect())
    }

    pub fn active_for_user(&self, user_id: i64) -> Result<Option<SessionDescriptor>> {
        let row = repository::transcripts::active_session_for_user(&self.db, user_id)?;
        Ok(row.as_ref().map(row_to_descriptor))
    }

    pub fn summary(&self, session_id: i64) -> Result<Option<SessionSummary>> {
        let row = repository::transcripts::get_session_summary(&self.db, session_id)?;
        Ok(row.map(|r| SessionSummary {
            session_id: r.session_id,
            tldr: r.tldr,
            decisions: r.decisions,
            action_items_json: r.action_items_json,
            open_questions: r.open_questions,
            model: r.model,
            generated_at: r.generated_at,
        }))
    }

    /// Zapisuje wygenerowane summary. Caller (handler) jest odpowiedzialny za
    /// wywołanie LLM i konstrukcję parsera action items.
    pub fn save_summary(
        &self,
        session_id: i64,
        tldr: &str,
        decisions: &str,
        action_items_json: &str,
        open_questions: &str,
        model: &str,
    ) -> Result<SessionSummary> {
        repository::transcripts::upsert_session_summary(
            &self.db,
            session_id,
            tldr,
            decisions,
            action_items_json,
            open_questions,
            model,
        )?;
        self.summary(session_id)?
            .ok_or_else(|| anyhow!("summary nie zapisane"))
    }

    pub fn db(&self) -> &DbPool {
        &self.db
    }
}

fn row_to_descriptor(row: &repository::transcripts::SessionRow) -> SessionDescriptor {
    SessionDescriptor {
        session_id: row.id,
        meeting_key: row.meeting_key.clone(),
        meeting_url: row.meeting_url.clone(),
        title: row.title.clone(),
        status: row.status.clone(),
        started_at: row.started_at.clone(),
        last_activity_at: row.last_activity_at.clone(),
        ended_at: row.ended_at.clone(),
        platform: row.platform.clone(),
        entry_count: row.entry_count,
        quic_port: row.quic_port.map(|p| p as u16),
        vnc_port: row.vnc_port.map(|p| p as u16),
        novnc_port: row.novnc_port.map(|p| p as u16),
        bot_endpoint_id: row.bot_endpoint_id.clone(),
        container_name: row.container_name.clone(),
        owner_user_id: row.owner_user_id,
        // Aliasy nie są persystowane w meeting_sessions — dla listy/detail
        // zwracamy defaulty. Rzeczywiste wartości są znane tylko w odpowiedzi
        // start_session (tuż po spawnie kontenera).
        stt_alias: DEFAULT_STT_ALIAS.to_string(),
        summarization_alias: DEFAULT_SUMMARIZATION_ALIAS.to_string(),
        tts_alias: DEFAULT_TTS_ALIAS.to_string(),
        flow_alias: DEFAULT_FLOW_ALIAS.to_string(),
    }
}

/// Rozwiązuje aliasy z requesta do konkretnych stringów, używając domyślnych
/// teams-* gdy caller nie nadpisze. Wyodrębnione żeby można było testować
/// niezależnie od spawna kontenera.
fn resolve_aliases(req: &StartSessionRequest) -> (String, String, String, String) {
    (
        req.stt_alias
            .clone()
            .unwrap_or_else(|| DEFAULT_STT_ALIAS.to_string()),
        req.summarization_alias
            .clone()
            .unwrap_or_else(|| DEFAULT_SUMMARIZATION_ALIAS.to_string()),
        req.tts_alias
            .clone()
            .unwrap_or_else(|| DEFAULT_TTS_ALIAS.to_string()),
        req.flow_alias
            .clone()
            .unwrap_or_else(|| DEFAULT_FLOW_ALIAS.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_aliases, StartSessionRequest, DEFAULT_FLOW_ALIAS, DEFAULT_STT_ALIAS,
        DEFAULT_SUMMARIZATION_ALIAS, DEFAULT_TTS_ALIAS,
    };
    use crate::db::migrations;
    use crate::db::repository;
    use crate::db::DbPool;
    use crate::services::teams_bot_bootstrap::ensure_teams_bot_defaults;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn make_req(
        stt: Option<&str>,
        sum: Option<&str>,
        tts: Option<&str>,
        flow: Option<&str>,
    ) -> StartSessionRequest {
        StartSessionRequest {
            meeting_url: "https://teams.example/meet".to_string(),
            title: None,
            platform: "teams".to_string(),
            owner_user_id: None,
            bot_name: "TF Bot".to_string(),
            stt_alias: stt.map(String::from),
            summarization_alias: sum.map(String::from),
            tts_alias: tts.map(String::from),
            flow_alias: flow.map(String::from),
        }
    }

    #[test]
    fn resolve_aliases_falls_back_to_teams_defaults() {
        let req = make_req(None, None, None, None);
        let (stt, sum, tts, flow) = resolve_aliases(&req);
        assert_eq!(stt, DEFAULT_STT_ALIAS);
        assert_eq!(sum, DEFAULT_SUMMARIZATION_ALIAS);
        assert_eq!(tts, DEFAULT_TTS_ALIAS);
        assert_eq!(flow, DEFAULT_FLOW_ALIAS);
    }

    #[test]
    fn resolve_aliases_honors_caller_overrides() {
        let req = make_req(Some("a"), Some("b"), Some("c"), Some("d"));
        let (stt, sum, tts, flow) = resolve_aliases(&req);
        assert_eq!(stt, "a");
        assert_eq!(sum, "b");
        assert_eq!(tts, "c");
        assert_eq!(flow, "d");
    }

    #[test]
    fn resolve_aliases_mixes_override_and_default() {
        let req = make_req(Some("custom-stt"), None, None, Some("custom-flow"));
        let (stt, sum, tts, flow) = resolve_aliases(&req);
        assert_eq!(stt, "custom-stt");
        assert_eq!(sum, DEFAULT_SUMMARIZATION_ALIAS);
        assert_eq!(tts, DEFAULT_TTS_ALIAS);
        assert_eq!(flow, "custom-flow");
    }

    fn setup_pool() -> DbPool {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations::run(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    // Symuluje scenariusz spawn-time: user ręcznie skasował alias między uruchomieniami.
    // Defensive hook w start_session() musi odtworzyć brakujący wpis bez błędu.
    #[tokio::test]
    async fn defensive_init_restores_deleted_alias() {
        let pool = setup_pool();

        ensure_teams_bot_defaults(&pool).await.unwrap();
        assert!(repository::resolve_model_alias(&pool, "teams-stt")
            .unwrap()
            .is_some());

        {
            let conn = pool.lock().unwrap();
            let deleted = conn
                .execute(
                    "DELETE FROM model_aliases WHERE alias = ?1",
                    rusqlite::params!["teams-stt"],
                )
                .unwrap();
            assert_eq!(deleted, 1);
        }
        assert!(repository::resolve_model_alias(&pool, "teams-stt")
            .unwrap()
            .is_none());

        ensure_teams_bot_defaults(&pool).await.unwrap();

        let restored = repository::resolve_model_alias(&pool, "teams-stt")
            .unwrap()
            .expect("alias teams-stt powinien zostać odtworzony");
        assert_eq!(restored.strategy.as_deref(), Some("first_available"));
    }
}
