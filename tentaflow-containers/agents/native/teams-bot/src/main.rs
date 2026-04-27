// =============================================================================
// Plik: main.rs
// Opis: Punkt wejscia sidecara meeting bot. Uruchamia pipeline: przegladarka,
//       przechwytywanie audio, VAD, serwer QUIC z reverse requestami STT/TTS.
// =============================================================================

mod audio;
mod browser;
mod config;
mod dom_observer;
mod quic_server;
mod summarizer;
mod vad;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

use crate::audio::RosterEntry;
use crate::config::MeetingConfig;
use crate::summarizer::{TranscriptBuffer, TranscriptEntry};
use crate::vad::VadResult;
use tentaflow_protocol::{MeetingEventPayload, LIFECYCLE_CONTAINER_SPAWNED, LIFECYCLE_FAILED};

/// Model diarization jest skonfigurowany hardcoded po stronie routera (mesh
/// config — jeden pipeline pyannote w tej wersji). Bot nie wie który konkretnie
/// jest używany, więc raportuje ustaloną nazwę — router przed broadcastem może
/// ją podmienić jeśli kiedyś pojawi się wybór.
const DIARIZATION_MODEL_NAME: &str = "pyannote-3.1";

/// Wysyła jednorazowy MeetingEventPayload::BackendUpdate na start sesji tak,
/// żeby dashboard pokazał jakie aliasy modeli są w grze. Pola opcjonalne
/// (liczby) zostają `None` — router może je wypełnić przed broadcastem
/// (enrolled_speakers z voice_profiles, total_participants z rostera).
async fn send_backend_update(
    router: &Arc<tokio::sync::Mutex<Option<Arc<quic_server::RouterClient>>>>,
    meeting_id: &str,
    config: &MeetingConfig,
) {
    let client = {
        let guard = router.lock().await;
        guard.as_ref().cloned()
    };
    let Some(client) = client else {
        tracing::warn!("send_backend_update: router client niedostepny");
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    if let Err(e) = client
        .send_meeting_event(
            meeting_id,
            ts,
            MeetingEventPayload::BackendUpdate {
                stt_model: config.stt_alias.clone(),
                tts_model: config.tts_alias.clone(),
                summarization_model: config.summarization_alias.clone(),
                diarization_model: DIARIZATION_MODEL_NAME.to_string(),
                streaming_latency_ms: None,
                enrolled_speakers: None,
                total_participants: None,
            },
        )
        .await
    {
        tracing::warn!("send_meeting_event BackendUpdate failed: {}", e);
    }
}

/// Generuje 2s 440Hz sine wave (16kHz mono i16 LE) i wysyla przez
/// audio_playback do JS mic injection. Diagnostyka pipeline'u TX
/// niezalezna od routera/TTS — uzywamy gdy chcemy zweryfikowac
/// MediaStreamTrackGenerator -> getUserMedia -> Teams MediaAgent ->
/// remote bez calego stacku TTS. Aktywuje sie env-em
/// `BOT_TEST_TONE_ON_JOIN=1`.
async fn play_test_tone(
    audio_playback: &Arc<crate::audio::AudioPlayback>,
    is_bot_speaking: &Arc<AtomicBool>,
) {
    const SR: u32 = 16_000;
    const FREQ: f64 = 440.0;
    const DUR_MS: u64 = 2_000;
    const AMP: f64 = 0.3 * (i16::MAX as f64);
    let n = (SR as u64 * DUR_MS / 1000) as usize;
    let mut pcm_bytes: Vec<u8> = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f64 / SR as f64;
        let s = (t * FREQ * 2.0 * std::f64::consts::PI).sin();
        let v = (s * AMP) as i16;
        pcm_bytes.extend_from_slice(&v.to_le_bytes());
    }
    is_bot_speaking.store(true, Ordering::Relaxed);
    match audio_playback.send(pcm_bytes) {
        Ok(()) => {
            tracing::info!(duration_ms = DUR_MS, freq = FREQ, "TEST tone wyslany do mic");
            let flag = Arc::clone(is_bot_speaking);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(DUR_MS + 250)).await;
                flag.store(false, Ordering::Relaxed);
            });
        }
        Err(e) => {
            tracing::warn!("play_test_tone: audio_playback.send failed: {}", e);
            is_bot_speaking.store(false, Ordering::Relaxed);
        }
    }
}

/// Announces the bot itself as a meeting participant. The DOM observer
/// (`dom_observer`) intentionally filters the bot row out, so without this
/// the GUI roster stays empty until a remote participant is detected.
/// Emitted once per join, right after `LIFECYCLE_JOINED`.
async fn send_bot_participant_joined(
    router: &Arc<tokio::sync::Mutex<Option<Arc<quic_server::RouterClient>>>>,
    meeting_id: &str,
    bot_name: &str,
) {
    let client = {
        let guard = router.lock().await;
        guard.as_ref().cloned()
    };
    let Some(client) = client else {
        tracing::debug!("send_bot_participant_joined: router client niedostepny");
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    if let Err(e) = client
        .send_meeting_event(
            meeting_id,
            ts,
            MeetingEventPayload::ParticipantUpdate {
                speaker_id: bot_name.to_string(),
                speaker_name: Some(bot_name.to_string()),
                status: "joined".to_string(),
                last_spoken_ago_sec: None,
            },
        )
        .await
    {
        tracing::warn!("send_meeting_event ParticipantUpdate(bot) failed: {}", e);
    }
}

/// Emituje pojedynczy `LifecycleUpdate` z main.rs. Browser.rs ma własny emitter
/// dla stage'y wewnątrz `join_meeting`; ta funkcja pokrywa stage'y na poziomie
/// main loop (container_spawned potwierdzenie przez bota, failed przy błędzie
/// join).
async fn emit_lifecycle(
    router: &Arc<tokio::sync::Mutex<Option<Arc<quic_server::RouterClient>>>>,
    meeting_id: &str,
    stage: &str,
    details: Option<String>,
) {
    let client = {
        let guard = router.lock().await;
        guard.as_ref().cloned()
    };
    let Some(client) = client else {
        tracing::debug!(stage, "emit_lifecycle: router client niedostepny — pomijam");
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    if let Err(e) = client
        .send_meeting_event(
            meeting_id,
            ts,
            MeetingEventPayload::LifecycleUpdate {
                stage: stage.to_string(),
                details,
            },
        )
        .await
    {
        tracing::warn!("emit_lifecycle({}) failed: {}", stage, e);
    }
}

/// Sprawdza czy ktorekolwiek z `wake_words` (CSV) wystepuje w tekscie
/// (case-insensitive, fragment slowa OK). Pusta lista wake_words = zawsze TRUE.
fn matches_wake_word(text: &str, wake_words_csv: &str) -> bool {
    let words: Vec<String> = wake_words_csv
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if words.is_empty() {
        return true;
    }
    let lower = text.to_lowercase();
    words.iter().any(|w| lower.contains(w.as_str()))
}

/// Pyta LLM-classifier czy wypowiedz to faktyczne pytanie do bota. Zwraca
/// true jezeli model zwrocil "TAK" (case-insensitive). Przy timeout/bledzie
/// fallback false — bezpieczniej nie odpowiadac niz halucynowac.
async fn intent_says_yes(
    client: &Arc<crate::quic_server::RouterClient>,
    config: &MeetingConfig,
    text: &str,
) -> bool {
    let messages = vec![
        ("system".to_string(), config.intent_prompt.clone()),
        ("user".to_string(), text.to_string()),
    ];
    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        client.chat_completion(&config.llm_alias, messages),
    )
    .await
    {
        Ok(Ok(resp)) => {
            let answer = resp.content.trim().to_uppercase();
            tracing::debug!(answer = %answer, "intent classifier zwrocil");
            answer.starts_with("TAK")
        }
        Ok(Err(e)) => {
            tracing::warn!("intent classifier failed: {}", e);
            false
        }
        Err(_) => {
            tracing::warn!("intent classifier timeout 10s");
            false
        }
    }
}

/// Decyduje czy bot odpowiada na wypowiedz, i jezeli tak — generuje odpowiedz
/// przez LLM. Zwraca tekst odpowiedzi albo None (bot milczy). Tryb gatowania
/// wybiera `config.response_mode`.
async fn gate_and_respond(
    config: &MeetingConfig,
    client: &Arc<crate::quic_server::RouterClient>,
    text: &str,
) -> Option<String> {
    // Etap 1: gating wedlug response_mode.
    let mode = config.response_mode.as_str();
    let allow = match mode {
        "always" => true,
        "wake_word" => matches_wake_word(text, &config.wake_words),
        "wake_word_intent" => {
            if !matches_wake_word(text, &config.wake_words) {
                false
            } else {
                tracing::info!(
                    text = %text.chars().take(60).collect::<String>(),
                    "wake_word match — pytam intent classifier"
                );
                intent_says_yes(client, config, text).await
            }
        }
        other => {
            tracing::warn!(mode = %other, "nieznany response_mode — domyslnie pasywny");
            false
        }
    };
    if !allow {
        return None;
    }

    // Etap 2: generuj odpowiedz przez LLM (alias `llm_alias`).
    let messages = vec![
        ("system".to_string(), config.response_prompt.clone()),
        ("user".to_string(), text.to_string()),
    ];
    match tokio::time::timeout(
        std::time::Duration::from_secs(20),
        client.chat_completion(&config.llm_alias, messages),
    )
    .await
    {
        Ok(Ok(resp)) => {
            let trimmed = resp.content.trim().to_string();
            if trimmed.is_empty() || trimmed.contains("<NO_RESPONSE>") {
                None
            } else {
                tracing::info!(
                    alias = %config.llm_alias,
                    resolved = %resp.resolved_model,
                    "LLM response: {}",
                    trimmed.chars().take(120).collect::<String>()
                );
                Some(trimmed)
            }
        }
        Ok(Err(e)) => {
            tracing::warn!("LLM response failed: {}", e);
            None
        }
        Err(_) => {
            tracing::warn!("LLM response timeout 20s");
            None
        }
    }
}

/// Uchwyt aktualnie uruchomionej petli summarizera dla sesji spotkania.
/// Przy LeaveMeeting / nowym JoinMeeting stary handle jest zamykany, a w jego
/// miejsce spawnujemy nowy — meeting_key musi pasowac do biezacej sesji.
struct SummarizerHandle {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    join: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl SummarizerHandle {
    async fn stop(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.join.await;
    }
}


/// Sidecar meeting bot — automatyzacja spotkan Teams z pipeline audio
#[derive(Parser, Debug)]
#[command(name = "tentaflow-meeting")]
#[command(about = "Sidecar do spotkan Teams z przechwytywaniem audio i transkrypcja")]
struct Args {
    /// Sciezka do pliku konfiguracji TOML
    #[arg(short, long, default_value = "meeting.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    // chromiumoxide::handler emits a torrent of "WS Invalid message: data did
    // not match any variant of untagged enum Message" warnings whenever
    // Chromium ships a CDP message variant the library does not yet know
    // about. Hundreds per second drown out our own logs. Default the crate
    // to error-level so RUST_LOG=info on the bot still works without the
    // noise; users can override with RUST_LOG=...,chromiumoxide=info if they
    // want the firehose back.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,chromiumoxide=error")),
        )
        .init();

    let args = Args::parse();

    // 1. Zaladuj konfiguracje
    let config = MeetingConfig::load(&args.config)?;
    tracing::info!(meeting_url = %config.meeting_url, "Konfiguracja zaladowana");

    // 2. Uruchom serwer iroh — router laczy sie do kontenera po EndpointId
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let transport_config = quic_server::ContainerTransportConfig {
        port: config.transport_port,
        secret_key_path: config.secret_key_path.clone(),
        secret_key_hex: config.secret_key_hex.clone(),
        enable_lan_discovery: config.enable_lan_discovery,
        enable_dht_discovery: config.enable_dht_discovery,
    };
    let quic = quic_server::MeetingQuicServer::new(transport_config);
    let transcript_tx = quic.transcript_sender();
    let router_client_handle = quic.router_client_handle();
    let page_slot = quic.page_slot();
    let mut command_rx = quic.command_receiver().await
        .expect("command_receiver powinien byc dostepny przy pierwszym wywolaniu");

    tokio::spawn(async move {
        if let Err(e) = quic.run(shutdown_rx).await {
            tracing::error!("Blad serwera iroh: {}", e);
        }
    });
    tracing::info!(port = config.transport_port, "Serwer iroh kontenera uruchomiony");

    // 3. Czekaj na polaczenie routera (potrzebujemy RouterClient do STT/TTS).
    // SKIP_ROUTER_WAIT=1 pozwala uruchomic bota w trybie dev bez routera —
    // Chromium i pipeline video startuja, audio capture do routera failuje
    // best-effort.
    if std::env::var("SKIP_ROUTER_WAIT").ok().as_deref() != Some("1") {
        tracing::info!("Czekam na polaczenie routera...");
        loop {
            let guard = router_client_handle.lock().await;
            if guard.is_some() {
                break;
            }
            drop(guard);
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
        tracing::info!("Router polaczony — STT/TTS dostepne");
    } else {
        tracing::warn!("SKIP_ROUTER_WAIT=1 — uruchamiam bota bez czekania na router (tryb dev)");
    }

    // SIGTERM/SIGINT handler — `docker stop` wysyla SIGTERM z 10s grace
    // przed SIGKILL. Bez tego goly browser.close() konczy proces zanim Teams
    // dostanie BYE/RTCP -> bot wisi w roster konferencji przez ~30s. Tutaj
    // klikamy Leave w Teams na aktywnej page (jesli istnieje), czekamy 1.5s
    // (tyle robi click_leave_in_teams) i wychodzimy. Browser zostanie
    // zatrzymany przez docker po naszym exit'cie — to zaplanowane.
    {
        let sig_page_slot = page_slot.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let mut term = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("SIGTERM signal setup failed: {}", e);
                    return;
                }
            };
            let mut intr = match signal(SignalKind::interrupt()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("SIGINT signal setup failed: {}", e);
                    return;
                }
            };
            tokio::select! {
                _ = term.recv() => tracing::info!("SIGTERM otrzymany — leave-first shutdown"),
                _ = intr.recv() => tracing::info!("SIGINT otrzymany — leave-first shutdown"),
            }
            let page = {
                let guard = sig_page_slot.lock().await;
                guard.clone()
            };
            if let Some(p) = page {
                match browser::click_leave_in_teams(&p).await {
                    Ok(true) => tracing::info!("Klikniete Leave w Teams (signal handler)"),
                    Ok(false) => tracing::warn!("Leave button niedostepny przy SIGTERM"),
                    Err(e) => tracing::warn!("click_leave_in_teams (signal): {}", e),
                }
            } else {
                tracing::info!("SIGTERM bez aktywnej sesji — wychodze bez kliku");
            }
            std::process::exit(0);
        });
    }

    // Rolling buffer transkrypcji — summarizer czyta go co N sekund.
    // Trzymany tu, a nie w summarizerze, zeby main loop mogl pushowac wpisy
    // niezaleznie od lifecycle summarizera (summarizer spawnuje sie per-session,
    // buffer zyje caly czas).
    let transcript_buffer: Arc<tokio::sync::Mutex<TranscriptBuffer>> = Arc::new(
        tokio::sync::Mutex::new(TranscriptBuffer::new(
            (config.transcript_buffer_minutes as i64).saturating_mul(60),
        )),
    );

    // Handle summarizera dla aktywnej sesji — None gdy bot nie jest w spotkaniu.
    let mut summarizer_handle: Option<SummarizerHandle> = None;
    // Handle skanera uczestnikow — analogicznie per-session.
    let mut dom_observer_handle: Option<dom_observer::DomObserver> = None;

    // Stan rosteru i aktywnego mowcy — zasilane przez dom_observer (push DOM
    // bridge), odczytywane w STT pipeline jako extra_meta. Trzymane na poziomie
    // calego sidecara, zeby przezyc LeaveMeeting / ponowny JoinMeeting bez
    // przedlokowania rzeczy ktore i tak dom_observer zaraz zaktualizuje.
    let current_roster: Arc<RwLock<Vec<RosterEntry>>> = Arc::new(RwLock::new(Vec::new()));
    let current_active_speaker: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    // Spawnuje summarizer dla podanego meeting_key. Stary handle (jesli byl)
    // musi byc wczesniej zamkniety przez caller — ta funkcja nie czysci.
    // Prompt pobierany jest z DB routera przez reverse QUIC (handler `PromptFetch`).
    // Gdy fetch nie powiedzie sie — zwracamy `None`, caller kontynuuje sesje
    // bez summarizera (transcript nadal dziala). Zadnego hardcoded fallbacku.
    async fn spawn_summarizer(
        buffer: Arc<tokio::sync::Mutex<TranscriptBuffer>>,
        router: Arc<tokio::sync::Mutex<Option<Arc<quic_server::RouterClient>>>>,
        meeting_key: String,
        config: &MeetingConfig,
    ) -> Option<SummarizerHandle> {
        // Pobierz aktualny RouterClient do fetchu promptu.
        let client = {
            let guard = router.lock().await;
            guard.as_ref().cloned()
        };
        let Some(client) = client else {
            tracing::warn!(
                "spawn_summarizer: router client niedostepny — skip summarizer"
            );
            return None;
        };

        let prompt_content = match client
            .fetch_prompt("transcription_summarization", &config.meeting_language)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    lang = %config.meeting_language,
                    "fetch_prompt nie powiodl sie — summarizer nie wystartuje w tej sesji"
                );
                return None;
            }
        };

        let (tx, rx) = tokio::sync::watch::channel(false);
        let alias = config.summarization_alias.clone();
        let interval = config.summarization_interval_sec;
        let min_entries = config.summarization_min_entries;
        let join = tokio::spawn(summarizer::run_summarizer_loop(
            buffer,
            router,
            interval,
            min_entries,
            meeting_key,
            alias,
            prompt_content,
            rx,
        ));
        Some(SummarizerHandle { shutdown_tx: tx, join })
    }

    // 3b. Most audio WebSocket — server musi byc UP zanim JS w Chromium
    //     pierwszy raz wykona connectWs(), inaczej widzimy spam "WS blad" /
    //     "WS zamkniety" do czasu az server zacznie nasluchiwac. Roster i
    //     active_speaker NIE ida tym kanalem (zasila je dom_observer).
    let (mut audio_capture, audio_playback) = audio::start_bridge().await?;
    let audio_playback = Arc::new(audio_playback);
    // Guard feedback-loop — gdy bot odtwarza TTS przez mic injection, capture
    // (z HTMLMediaElement i pc.ontrack) moze ponownie zlapac ten sam glos
    // przez echo konferencji. Bez tego STT zapetla sie na wlasnych odpowiedziach.
    let is_bot_speaking = Arc::new(AtomicBool::new(false));
    tracing::info!("Most audio WebSocket uruchomiony na 127.0.0.1:9999");

    // 4. Uruchom przegladarke i dolacz do spotkania (jesli URL podany)
    let mut _chromium: Option<chromiumoxide::browser::Browser> = None;
    let mut page = if !config.meeting_url.is_empty() {
        // meeting_id potrzebujemy zanim wywolamy browser::join_meeting —
        // lifecycle events (browser_launched / navigating / prejoin_ready /
        // joining / joined) beda wysylane z meeting_key = meeting_id. Router
        // po stronie hosta utworzyl juz wpis meeting_sessions pod tym kluczem
        // (przez MEETING_ID env), wiec LifecycleUpdate trafi do wlasciwej sesji.
        let meeting_id = config
            .meeting_id_override
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        {
            let guard = router_client_handle.lock().await;
            if let Some(ref c) = *guard {
                c.set_meeting_id(meeting_id.clone());
            }
        }

        // Bot potwierdza że żyje i gada z routerem. Manager hosta wstawil
        // `container_spawned` po udanym `docker start`, ale ten event jest
        // prawdziwym potwierdzeniem z perspektywy bota.
        emit_lifecycle(
            &router_client_handle,
            &meeting_id,
            LIFECYCLE_CONTAINER_SPAWNED,
            None,
        )
        .await;

        let browser = browser::launch_chromium(&config).await?;
        let (p, observer) = match browser::join_meeting(
            &browser,
            &config.meeting_url,
            &config,
            &router_client_handle,
            &meeting_id,
            Arc::clone(&current_roster),
            Arc::clone(&current_active_speaker),
        )
        .await
        {
            Ok(pair) => pair,
            Err(e) => {
                emit_lifecycle(
                    &router_client_handle,
                    &meeting_id,
                    LIFECYCLE_FAILED,
                    Some(format!("{e}")),
                )
                .await;
                return Err(e);
            }
        };
        _chromium = Some(browser);
        {
            let mut slot = page_slot.lock().await;
            *slot = Some(p.clone());
        }
        tracing::info!("Dolaczono do spotkania");
        summarizer_handle = spawn_summarizer(
            transcript_buffer.clone(),
            router_client_handle.clone(),
            meeting_id.clone(),
            &config,
        )
        .await;
        dom_observer_handle = Some(observer);
        send_backend_update(&router_client_handle, &meeting_id, &config).await;
        send_bot_participant_joined(&router_client_handle, &meeting_id, &config.bot_name).await;
        if std::env::var("BOT_TEST_TONE_ON_JOIN").ok().as_deref() == Some("1") {
            // 1.5s opoznienie zeby Teams MediaAgent zdazyl podlaczyc track
            // przed pierwszym AudioData write — bez tego frames trafiaja do
            // void zanim peer connection zaczyna ich uzywac.
            let pb = Arc::clone(&audio_playback);
            let speaking = Arc::clone(&is_bot_speaking);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                play_test_tone(&pb, &speaking).await;
            });
        }
        Some(p)
    } else {
        tracing::info!("Brak meeting_url — kontener czeka na komende join przez QUIC");
        None
    };

    // 6. Inicjalizacja VAD
    let mut vad_detector = vad::VadDetector::new(
        config.vad_model_path.as_deref(),
        config.chunk_duration_ms,
        config.silence_threshold_ms,
        config.vad_rms_threshold,
    )?;

    // Diarization jest wykonywana po stronie routera (tentaflow-core) razem ze STT.
    // Bot otrzymuje juz etykiete speakera w ModelResponse. Jesli router nie zwroci
    // speaker_label, uzywamy fallback "Nieznany".

    // 7. Glowna petla: audio -> VAD -> STT (QUIC) -> streaming transcript -> TTS (QUIC)
    let mut speech_buffer: Vec<i16> = Vec::new();
    let stt_alias = config.stt_alias.as_str();
    let tts_alias = config.tts_alias.as_str();

    tracing::info!("Pipeline audio uruchomiony (STT alias: {}, TTS alias: {})", stt_alias, tts_alias);

    // Bulletproof audio segmentation dla STT + WeSpeaker diarization.
    //
    // Strategia (ustawione mocno, nie majsterkowac bez mierzenia):
    //   - 16 kHz mono PCM i16
    //   - MAX bufora speech: 15s (optymalne dla Whispera)
    //   - Silence hysteresis: 600 ms (naturalna granica zdania)
    //   - Pre-padding: 250 ms (kontekst dla Whispera na poczatku)
    //   - Post-padding: 250 ms (koncowki slow)
    //   - Minimum segment do wyslania: 1.5s (WeSpeaker potrzebuje tyle na
    //     wiarygodny embedding, a Whisper jest zbyt niestabilny na krotsze)
    //   - SNR gate: >= 10 dB (ponizej to szum/cisza, nie wyslamy)
    //
    // Krotsze "starts/stops" mowy (<1.5s) sa buforowane z poprzednim segmentem
    // zeby nie tworzyc mikro-chunkow ktore degradujaa i Whispera i diarization.
    const SAMPLE_RATE: usize = 16_000;
    const MAX_SPEECH_SAMPLES: usize = 15 * SAMPLE_RATE;           // 15s hard cap
    const MIN_SEGMENT_SAMPLES: usize = (1.5 * SAMPLE_RATE as f32) as usize; // 1.5s minimum
    const PREPAD_SAMPLES: usize = SAMPLE_RATE / 4;                // 250 ms
    const POSTPAD_SAMPLES: usize = SAMPLE_RATE / 4;               // 250 ms
    const MIN_SNR_DB: f32 = 10.0;

    // Ring buffer pre-padding — zawsze trzymamy ostatnie 250ms audio zeby
    // dolaczyc je gdy VAD wykryje poczatek mowy. Dzieki temu Whisper dostaje
    // troche kontekstu przed pierwszym slowem.
    let mut prepad_buffer: std::collections::VecDeque<i16> =
        std::collections::VecDeque::with_capacity(PREPAD_SAMPLES);

    // Licznik ciszy po mowie — zbiera POSTPAD_SAMPLES ciszy zeby wyslac
    // segment z zakonczeniem slow.
    let mut silence_tail_samples: usize = 0;
    let mut collecting_silence_tail = false;

    let mut chunk_count: u64 = 0;

    /// Oblicza SNR (dB) dla i16 PCM — proxy heurystyka (p90 vs p10 rms frames).
    fn snr_db_from_i16(pcm: &[i16]) -> f32 {
        if pcm.len() < 1600 {
            return 0.0;
        }
        let frame = 1600;
        let n = pcm.len() / frame;
        if n < 4 { return 0.0; }
        let mut rms: Vec<f32> = (0..n).map(|i| {
            let s = &pcm[i * frame..(i + 1) * frame];
            let ss: f64 = s.iter().map(|x| (*x as f64).powi(2)).sum();
            ((ss / frame as f64).sqrt()) as f32
        }).collect();
        rms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p10 = rms[n / 10].max(1.0);
        let p90 = rms[(n * 9) / 10];
        20.0 * (p90 / p10).log10()
    }

    loop {
        tokio::select! {
            chunk = audio_capture.next_chunk() => {
                let Some(chunk) = chunk else {
                    tracing::warn!("Strumien audio zakonczony");
                    break;
                };

                // Gdy bot aktualnie odtwarza TTS, ignorujemy chunk — inaczej
                // echo konferencji zamyka petle feedback: bot mowi -> slyszy siebie -> STT -> TTS.
                if is_bot_speaking.load(Ordering::Relaxed) {
                    continue;
                }

                chunk_count += 1;

                let rms = (chunk.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / chunk.len() as f64).sqrt();
                let vad_result = vad_detector.process_chunk(&chunk);

                // Aktualizuj ring buffer pre-pad niezaleznie od VAD
                for &s in chunk.iter() {
                    if prepad_buffer.len() >= PREPAD_SAMPLES {
                        prepad_buffer.pop_front();
                    }
                    prepad_buffer.push_back(s);
                }

                tracing::debug!(
                    chunk = chunk_count,
                    rms = format!("{:.0}", rms),
                    buf = speech_buffer.len(),
                    tail = silence_tail_samples,
                    "{:?}",
                    vad_result
                );

                let mut should_send = false;

                match vad_result {
                    VadResult::Speech => {
                        // Poczatek mowy po ciszy — dolacz pre-pad
                        if speech_buffer.is_empty() {
                            speech_buffer.extend(prepad_buffer.iter().copied());
                        }
                        speech_buffer.extend_from_slice(&chunk);
                        collecting_silence_tail = false;
                        silence_tail_samples = 0;

                        if speech_buffer.len() >= MAX_SPEECH_SAMPLES {
                            tracing::info!(
                                buf_samples = speech_buffer.len(),
                                "Max speech buffer (15s) — force STT"
                            );
                            should_send = true;
                        }
                    }
                    VadResult::Transition => {
                        // VAD zdecydowal ze mowa sie skonczyla. Dopuszczamy chunk
                        // do bufora (moze zawierac koncowke slowa) + zaczynamy
                        // zbierac post-pad ciszy.
                        speech_buffer.extend_from_slice(&chunk);
                        collecting_silence_tail = true;
                        silence_tail_samples = chunk.len();
                        tracing::debug!(
                            buf_samples = speech_buffer.len(),
                            "VAD Transition — zbieram post-pad"
                        );
                    }
                    VadResult::Silence => {
                        if collecting_silence_tail {
                            // Dobierz post-pad z chunka az do POSTPAD_SAMPLES
                            let need = POSTPAD_SAMPLES.saturating_sub(silence_tail_samples);
                            let take = need.min(chunk.len());
                            if take > 0 {
                                speech_buffer.extend_from_slice(&chunk[..take]);
                                silence_tail_samples += take;
                            }
                            if silence_tail_samples >= POSTPAD_SAMPLES {
                                should_send = true;
                                collecting_silence_tail = false;
                            }
                        }
                        // W przeciwnym razie to cisza przed mowa — nic nie robimy,
                        // ring buffer prepad juz kolekcjonuje.
                    }
                }

                if should_send && !speech_buffer.is_empty() {
                    // Quality gate 1: minimum duration
                    if speech_buffer.len() < MIN_SEGMENT_SAMPLES {
                        tracing::debug!(
                            samples = speech_buffer.len(),
                            min = MIN_SEGMENT_SAMPLES,
                            "Segment za krotki — hold dla kolejnej mowy"
                        );
                        // NIE czyscimy bufora — kolejna faza Speech dolaczy do tego
                        // (wtedy drugi "start" pomija prepad bo buffer nie jest pusty).
                        collecting_silence_tail = false;
                        silence_tail_samples = 0;
                        continue;
                    }

                    // Quality gate 2: SNR
                    let snr = snr_db_from_i16(&speech_buffer);
                    if snr < MIN_SNR_DB {
                        tracing::info!(
                            snr_db = snr,
                            samples = speech_buffer.len(),
                            "Segment odrzucony — za niski SNR"
                        );
                        speech_buffer.clear();
                        collecting_silence_tail = false;
                        silence_tail_samples = 0;
                        continue;
                    }

                    let timestamp_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let duration_ms = (speech_buffer.len() * 1000) / SAMPLE_RATE;
                    tracing::info!(
                        samples = speech_buffer.len(),
                        duration_ms,
                        snr_db = snr,
                        model = stt_alias,
                        "Wysylam segment do STT"
                    );

                    // Doklej kontekst spotkania do metadata STT — router uzywa
                    // go do diarization, logow i ewentualnej generacji odpowiedzi.
                    // Sanityzacja: obcinamy znaki kontrolne i limitujemy dlugosc zeby
                    // zlosliwa/zbugowana strona Teams nie wstrzyknela metadata-bomb.
                    let mut extra_meta: Vec<(String, String)> = Vec::new();
                    {
                        let roster_snapshot = current_roster.read().await;
                        let names: Vec<String> = roster_snapshot
                            .iter()
                            .take(50)
                            .map(|r| r.name.chars()
                                .filter(|c| !c.is_control())
                                .take(128)
                                .collect::<String>())
                            .filter(|s| !s.is_empty())
                            .collect();
                        drop(roster_snapshot);
                        if !names.is_empty() {
                            if let Ok(json) = serde_json::to_string(&names) {
                                extra_meta.push(("roster".to_string(), json));
                            }
                        }
                    }
                    if let Some(ref speaker) = *current_active_speaker.read().await {
                        let cleaned: String = speaker.chars()
                            .filter(|c| !c.is_control())
                            .take(128)
                            .collect();
                        if !cleaned.is_empty() {
                            extra_meta.push(("active_speaker".to_string(), cleaned));
                        }
                    }
                    extra_meta.push(("timestamp_ms".to_string(), timestamp_ms.to_string()));

                    // Pobierz klienta raz dla calej iteracji (STT + opcjonalny TTS) —
                    // osobny lock dla TTS moglby trafic na inny Arc jesli router sie
                    // zrekonektowal miedzy wywolaniami.
                    let client = {
                        let guard = router_client_handle.lock().await;
                        guard.as_ref().cloned()
                    };
                    let Some(client) = client else {
                        tracing::warn!("router client not available, skipping STT");
                        speech_buffer.clear();
                        collecting_silence_tail = false;
                        silence_tail_samples = 0;
                        continue;
                    };
                    let stt_started = std::time::Instant::now();
                    let stt_result = match tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        client.transcribe(&speech_buffer, stt_alias, None, extra_meta),
                    ).await {
                        Ok(result) => result,
                        Err(_) => {
                            tracing::warn!("STT timeout po 30s — router nie odpowiada");
                            Err(anyhow::anyhow!("STT timeout"))
                        }
                    };
                    let stt_latency_ms = stt_started.elapsed().as_millis() as u64;
                    match stt_result {
                        Ok(text) if !text.is_empty() => {
                            tracing::info!(text = %text, latency_ms = stt_latency_ms, "STT zwrocilo transkrypt");
                            let _ = transcript_tx.send(("Nieznany".to_string(), text.clone(), timestamp_ms));

                            // Wpis do rolling bufferu summarizera. Speaker name
                            // pochodzi z active_speaker DOM (gdy dostepny) — bez
                            // diarization po stronie bota uzywamy fallback "Nieznany",
                            // ktory i tak pojdzie do prompta jako `[Nieznany]`.
                            let speaker_label = current_active_speaker
                                .read()
                                .await
                                .clone()
                                .unwrap_or_else(|| "Nieznany".to_string());
                            {
                                let mut buf = transcript_buffer.lock().await;
                                buf.push(TranscriptEntry {
                                    timestamp_ms: timestamp_ms as i64,
                                    speaker_name: speaker_label.clone(),
                                    text: text.clone(),
                                });
                            }

                            // Live broadcast chunku transkryptu — router rozsyła
                            // do dashboardu i może wzbogacić speaker_id (diarization
                            // lookup z voice_profiles). Persist chunka do DB leci
                            // osobno przez STT metadata `meeting_id` → transcript_store,
                            // więc ten event nie duplikuje zapisu.
                            if let Some(meeting_id) = client.current_meeting_id() {
                                if let Err(e) = client
                                    .send_meeting_event(
                                        &meeting_id,
                                        timestamp_ms as i64,
                                        MeetingEventPayload::TranscriptEntry {
                                            speaker_id: speaker_label.clone(),
                                            speaker_name: if speaker_label == "Nieznany" {
                                                None
                                            } else {
                                                Some(speaker_label.clone())
                                            },
                                            is_enrolled: false,
                                            speaker_confidence: None,
                                            text: text.clone(),
                                            language: None,
                                            resolved_stt_model: stt_alias.to_string(),
                                            latency_ms: stt_latency_ms,
                                        },
                                    )
                                    .await
                                {
                                    tracing::warn!(
                                        "send_meeting_event TranscriptEntry failed: {}",
                                        e
                                    );
                                }
                            }

                            // Pelny pipeline z gatowaniem aktywacji:
                            //   1. echo_mode -> bot powtarza dokladnie (test)
                            //   2. respond_enabled + llm_alias wpiety
                            //   3. response_mode determinuje kiedy odpalamy LLM:
                            //        - "always": kazda wypowiedz
                            //        - "wake_word": tylko gdy wake_word w tekscie
                            //        - "wake_word_intent" (default): wake_word + LLM-classifier
                            //   4. response generation: LLM moze zwrocic <NO_RESPONSE>
                            //
                            // Pasywny tryb (default `wake_word_intent`) gwarantuje
                            // 0 LLM calls dla normalnej rozmowy bez wezwania bota.
                            let response_text: Option<String> = if config.echo_mode {
                                Some(text.clone())
                            } else if config.respond_enabled
                                && !config.llm_alias.trim().is_empty()
                            {
                                gate_and_respond(
                                    &config,
                                    &client,
                                    &text,
                                ).await
                            } else {
                                tracing::info!(
                                    respond_enabled = config.respond_enabled,
                                    llm_alias_empty = config.llm_alias.trim().is_empty(),
                                    "skip LLM response (bot pasywny — sprawdz respond_enabled / llm_alias)"
                                );
                                None
                            };
                            if let Some(reply) = response_text {
                                match tokio::time::timeout(
                                    std::time::Duration::from_secs(30),
                                    client.synthesize(&reply, "", tts_alias),
                                ).await {
                                    Ok(Ok(audio_bytes)) => {
                                        match audio::parse_audio_payload(audio_bytes)
                                            .context("parse tts wav")
                                        {
                                            Ok(pcm) => {
                                                // Zablokuj capture na czas odtwarzania (len bajtow / 2 bps / sr).
                                                let duration_ms = (pcm.len() as u64) * 1000 / (2 * 16_000);
                                                is_bot_speaking.store(true, Ordering::Relaxed);
                                                if let Err(e) = audio_playback.send(pcm) {
                                                    tracing::warn!("Nie udalo sie wyslac TTS do mic bota: {}", e);
                                                    is_bot_speaking.store(false, Ordering::Relaxed);
                                                } else {
                                                    tracing::info!(duration_ms, "TTS wyslany do mikrofonu bota");
                                                    let flag = Arc::clone(&is_bot_speaking);
                                                    tokio::spawn(async move {
                                                        tokio::time::sleep(std::time::Duration::from_millis(duration_ms + 250)).await;
                                                        flag.store(false, Ordering::Relaxed);
                                                    });
                                                }
                                            }
                                            Err(e) => tracing::warn!("TTS WAV malformed: {:#}", e),
                                        }
                                    }
                                    Ok(Err(e)) => tracing::warn!("Blad TTS: {}", e),
                                    Err(_) => tracing::warn!("TTS timeout po 30s"),
                                }
                            }
                        }
                        Ok(_) => {
                            tracing::debug!("STT zwrocil pusty tekst — pomijam");
                        }
                        Err(e) => {
                            tracing::warn!("Blad STT: {} — pomijam segment", e);
                        }
                    }
                    speech_buffer.clear();
                    collecting_silence_tail = false;
                    silence_tail_samples = 0;
                }
            }
            cmd = command_rx.recv() => {
                match cmd {
                    Some(quic_server::MeetingCommand::JoinMeeting { meeting_url, response_tx }) => {
                        tracing::info!(meeting_url = %meeting_url, "Komenda QUIC: dolaczanie do spotkania");

                        // Zamknij stara przegladarke przed uruchomieniem nowej —
                        // inaczej zablokowany user_data_dir lub stare procesy wisza
                        if let Some(mut old_browser) = _chromium.take() {
                            tracing::info!("Zamykam poprzednia instancje Chromium");
                            page = None;
                            {
                                let mut slot = page_slot.lock().await;
                                *slot = None;
                            }
                            let _ = old_browser.close().await;
                            let _ = old_browser.wait().await;
                        }
                        // Zatrzymaj summarizer poprzedniej sesji — nowe spotkanie
                        // dostanie swojego z nowym meeting_key. Buffer czyscimy zeby
                        // transkrypty z poprzedniego spotkania nie trafily do promptu.
                        if let Some(h) = summarizer_handle.take() {
                            h.stop().await;
                        }
                        if let Some(h) = dom_observer_handle.take() {
                            h.stop().await;
                        }
                        transcript_buffer.lock().await.clear();
                        speech_buffer.clear();
                        vad_detector.reset();

                        // meeting_id: gdy MeetingManager na hoscie pasuje env MEETING_ID
                        // (config.meeting_id_override), uzywamy go — dzieki temu router
                        // zapisuje transkrypty pod ta sama sesja ktora manager utworzyl
                        // w meeting_sessions. Gdy brak env — fallback do uuid (stand-alone
                        // tryb, bez host manager).
                        let meeting_id = config
                            .meeting_id_override
                            .clone()
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                        tracing::info!(
                            meeting_id = %meeting_id,
                            from_host = config.meeting_id_override.is_some(),
                            "meeting_id ustawione"
                        );
                        {
                            let client = router_client_handle.lock().await;
                            if let Some(ref c) = *client {
                                c.set_meeting_id(meeting_id.clone());
                            }
                        }

                        // Bot potwierdza container_spawned przed launch'em — GUI
                        // rozrozni "host spawnowal kontener" od "bot zaczal join".
                        emit_lifecycle(
                            &router_client_handle,
                            &meeting_id,
                            LIFECYCLE_CONTAINER_SPAWNED,
                            None,
                        )
                        .await;
                        match browser::launch_chromium(&config).await {
                            Ok(browser) => {
                                match browser::join_meeting(
                                    &browser,
                                    &meeting_url,
                                    &config,
                                    &router_client_handle,
                                    &meeting_id,
                                    Arc::clone(&current_roster),
                                    Arc::clone(&current_active_speaker),
                                )
                                .await
                                {
                                    Ok((p, observer)) => {
                                        _chromium = Some(browser);
                                        {
                                            let mut slot = page_slot.lock().await;
                                            *slot = Some(p.clone());
                                        }
                                        page = Some(p.clone());
                                        // Odpal summarizera dla nowej sesji —
                                        // meeting_key == meeting_id po stronie routera.
                                        summarizer_handle = spawn_summarizer(
                                            transcript_buffer.clone(),
                                            router_client_handle.clone(),
                                            meeting_id.clone(),
                                            &config,
                                        )
                                        .await;
                                        dom_observer_handle = Some(observer);
                                        send_backend_update(
                                            &router_client_handle,
                                            &meeting_id,
                                            &config,
                                        )
                                        .await;
                                        send_bot_participant_joined(
                                            &router_client_handle,
                                            &meeting_id,
                                            &config.bot_name,
                                        )
                                        .await;
                                        let _ = response_tx.send(format!(

                                            "OK: dolaczono do spotkania (meeting_id={})",
                                            meeting_id
                                        ));
                                    }
                                    Err(e) => {
                                        let err_msg = format!("{}", e);
                                        emit_lifecycle(
                                            &router_client_handle,
                                            &meeting_id,
                                            LIFECYCLE_FAILED,
                                            Some(err_msg.clone()),
                                        )
                                        .await;
                                        // Wyczysc meeting_id bo join sie nie udal
                                        let client = router_client_handle.lock().await;
                                        if let Some(ref c) = *client {
                                            c.clear_meeting_id();
                                        }
                                        let _ = response_tx.send(format!("BLAD: {}", err_msg));
                                    }
                                }
                            }
                            Err(e) => {
                                let err_msg = format!("nie udalo sie uruchomic przegladarki: {}", e);
                                emit_lifecycle(
                                    &router_client_handle,
                                    &meeting_id,
                                    LIFECYCLE_FAILED,
                                    Some(err_msg.clone()),
                                )
                                .await;
                                let client = router_client_handle.lock().await;
                                if let Some(ref c) = *client {
                                    c.clear_meeting_id();
                                }
                                let _ = response_tx.send(format!("BLAD: {}", err_msg));
                            }
                        }
                    }
                    Some(quic_server::MeetingCommand::LeaveMeeting { response_tx }) => {
                        tracing::info!("Komenda QUIC: opuszczanie spotkania");
                        // Najpierw klikamy Leave w Teams zeby konferencja
                        // dostala BYE/RTCP — bez tego Teams trzyma bota w
                        // roster przez ~30s po samym docker stop.
                        if let Some(ref p) = page {
                            match browser::click_leave_in_teams(p).await {
                                Ok(true) => tracing::info!("Klikniete Leave w Teams"),
                                Ok(false) => tracing::warn!(
                                    "Leave button niedostepny — zamykam Chromium bez kliku"),
                                Err(e) => tracing::warn!(
                                    "click_leave_in_teams failed: {} — zamykam Chromium", e),
                            }
                        }
                        page = None;
                        {
                            let mut slot = page_slot.lock().await;
                            *slot = None;
                        }
                        if let Some(mut old_browser) = _chromium.take() {
                            let _ = old_browser.close().await;
                            let _ = old_browser.wait().await;
                        }
                        if let Some(h) = summarizer_handle.take() {
                            h.stop().await;
                        }
                        if let Some(h) = dom_observer_handle.take() {
                            h.stop().await;
                        }
                        transcript_buffer.lock().await.clear();
                        speech_buffer.clear();
                        vad_detector.reset();

                        // Wyczysc meeting_id — nastepne STT beda bez meetingu
                        {
                            let client = router_client_handle.lock().await;
                            if let Some(ref c) = *client {
                                c.clear_meeting_id();
                            }
                        }

                        let _ = response_tx.send("OK: opuszczono spotkanie".to_string());
                    }
                    Some(quic_server::MeetingCommand::GetStatus { response_tx }) => {
                        let status = if page.is_some() { "connected" } else { "idle" };
                        let _ = response_tx.send(status.to_string());
                    }
                    None => {
                        tracing::warn!("Kanal komend zamkniety");
                        break;
                    }
                }
            }
        }

        // Sprawdz czy autoryzacja nie wygasla
        if let Some(ref p) = page {
            if let Ok(true) = browser::detect_auth_expired(p).await {
                tracing::error!("Autoryzacja Teams wygasla — koncze sidecar");
                break;
            }
        }
    }

    if let Some(h) = summarizer_handle.take() {
        h.stop().await;
    }
    if let Some(h) = dom_observer_handle.take() {
        h.stop().await;
    }
    let _ = shutdown_tx.send(true);
    tracing::info!("Sidecar meeting bot zakonczony");
    Ok(())
}
