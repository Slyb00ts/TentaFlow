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
    pub tts_alias: Option<String>,
    pub llm_alias: Option<String>,
}

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
}

impl MeetingManager {
    pub fn new(db: DbPool) -> Arc<Self> {
        Arc::new(Self { db })
    }

    /// Startuje nową sesję Meeting Bot. Flow:
    /// 1. INSERT meeting_sessions (status=idle)
    /// 2. Alokuj trójkę portów (quic/vnc/novnc)
    /// 3. Wygeneruj Ed25519 secret key bota, oblicz endpoint_id (public key hex)
    /// 4. Zaktualizuj sesję (status=joining, container info, ports, keys)
    /// 5. Spawn kontener z env
    /// Jeśli którykolwiek krok zawiedzie — cofnij i zwroc blad.
    pub async fn start_session(
        &self,
        req: StartSessionRequest,
    ) -> Result<SessionDescriptor> {
        // meeting_key = hash url + nanos, żeby ponowne dołączanie do tego samego
        // URL nie kolidowało (każde spotkanie = nowa sesja).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let meeting_key = format!("mtg-{}", nanos);

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

        let spawn_req = SpawnRequest {
            session_id,
            meeting_url: req.meeting_url.clone(),
            ports,
            secret_key_hex: secret_key_hex.clone(),
            bot_name: req.bot_name.clone(),
            stt_alias: req.stt_alias.clone(),
            tts_alias: req.tts_alias.clone(),
            llm_alias: req.llm_alias.clone(),
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

        self.session_detail(session_id)?
            .ok_or_else(|| anyhow!("nie udalo sie pobrac sesji po spawnie"))
    }

    /// Zatrzymuje sesję: stop+rm kontener, release portów, status=ended.
    pub async fn leave_session(&self, session_id: i64) -> Result<()> {
        repository::transcripts::set_session_status(&self.db, session_id, "leaving")?;
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

    pub fn session_list(
        &self,
        owner_user_id: Option<i64>,
    ) -> Result<Vec<SessionDescriptor>> {
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
    }
}
