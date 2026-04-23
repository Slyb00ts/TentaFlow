// =============================================================================
// Plik: main.rs
// Opis: Punkt wejscia sidecara meeting bot. Uruchamia pipeline: przegladarka,
//       przechwytywanie audio, VAD, serwer QUIC z reverse requestami STT/TTS.
// =============================================================================

mod audio;
mod browser;
mod config;
mod quic_server;
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
use crate::vad::VadResult;

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
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
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
    let mut command_rx = quic.command_receiver().await
        .expect("command_receiver powinien byc dostepny przy pierwszym wywolaniu");

    tokio::spawn(async move {
        if let Err(e) = quic.run(shutdown_rx).await {
            tracing::error!("Blad serwera iroh: {}", e);
        }
    });
    tracing::info!(port = config.transport_port, "Serwer iroh kontenera uruchomiony");

    // 3. Czekaj na polaczenie routera (potrzebujemy RouterClient do STT/TTS)
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

    // 4. Uruchom przegladarke i dolacz do spotkania (jesli URL podany)
    let mut _chromium: Option<chromiumoxide::browser::Browser> = None;
    let mut page = if !config.meeting_url.is_empty() {
        let browser = browser::launch_chromium(&config).await?;
        let p = browser::join_meeting(&browser, &config.meeting_url, &config).await?;
        _chromium = Some(browser);
        tracing::info!("Dolaczono do spotkania");
        Some(p)
    } else {
        tracing::info!("Brak meeting_url — kontener czeka na komende join przez QUIC");
        None
    };

    // 5. Uruchom most audio WebSocket (wstrzykniety JS w Chromium <-> Rust)
    // Zastepuje wczesniejsze parec/pacat — audio przechwytywane na poziomie
    // HTMLMediaElement w Chromium przez captureStream(), bez PulseAudio monitor.
    let (mut audio_capture, audio_playback, mut roster_rx, mut speaker_rx) =
        audio::start_bridge().await?;
    let audio_playback = Arc::new(audio_playback);
    // Guard feedback-loop — gdy bot odtwarza TTS przez mic injection, capture
    // (z HTMLMediaElement i pc.ontrack) moze ponownie zlapac ten sam glos
    // przez echo konferencji. Bez tego STT zapetla sie na wlasnych odpowiedziach.
    let is_bot_speaking = Arc::new(AtomicBool::new(false));
    tracing::info!("Most audio WebSocket uruchomiony na 127.0.0.1:9999");

    // Stan rostera i aktywnego mowcy dzielony z taskami receiverow.
    // Roster wysylany jest w metadata STT, active_speaker tak samo.
    let current_roster: Arc<RwLock<Vec<RosterEntry>>> = Arc::new(RwLock::new(Vec::new()));
    let current_active_speaker: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    {
        let roster_state = Arc::clone(&current_roster);
        tokio::spawn(async move {
            // Telemetria selektorow — jesli DOM Teams sie zmieni, roster bedzie
            // pusty; pod 3 pod rzad ostrzegamy (moze pomoc w diagnostyce bez SSH).
            let mut empty_count: u32 = 0;
            while let Some(list) = roster_rx.rx.recv().await {
                if list.is_empty() {
                    empty_count = empty_count.saturating_add(1);
                    if empty_count >= 3 {
                        tracing::warn!(
                            empty_count,
                            "roster empty {} times — Teams DOM may have changed",
                            empty_count
                        );
                    }
                } else {
                    empty_count = 0;
                }
                *roster_state.write().await = list;
            }
        });
    }
    {
        let speaker_state = Arc::clone(&current_active_speaker);
        tokio::spawn(async move {
            let mut none_count: u32 = 0;
            while let Some(name) = speaker_rx.rx.recv().await {
                if name.is_none() {
                    none_count = none_count.saturating_add(1);
                    if none_count == 10 {
                        tracing::warn!(
                            none_count,
                            "active_speaker None {} times — check data-is-presenter selector",
                            none_count
                        );
                    }
                } else {
                    none_count = 0;
                }
                *speaker_state.write().await = name;
            }
        });
    }

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
                    match stt_result {
                        Ok(text) if !text.is_empty() => {
                            tracing::info!(text = %text, "STT zwrocilo transkrypt");
                            let _ = transcript_tx.send(("Nieznany".to_string(), text.clone(), timestamp_ms));

                            // TTS uruchamiamy TYLKO w echo_mode (tryb testowy). Bez tego
                            // bot powtarza co uslyszal = nieskonczony feedback loop, nawet
                            // ze self-speaking guardem. Docelowo LLM response pojdzie osobnym
                            // polem w ModelResponse i wtedy to bedzie branch na llm_response.
                            if config.echo_mode {
                                match tokio::time::timeout(
                                    std::time::Duration::from_secs(30),
                                    client.synthesize(&text, "", tts_alias),
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
                            let _ = old_browser.close().await;
                            let _ = old_browser.wait().await;
                        }
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

                        match browser::launch_chromium(&config).await {
                            Ok(browser) => {
                                match browser::join_meeting(&browser, &meeting_url, &config).await {
                                    Ok(p) => {
                                        _chromium = Some(browser);
                                        page = Some(p);
                                        let _ = response_tx.send(format!(
                                            "OK: dolaczono do spotkania (meeting_id={})",
                                            meeting_id
                                        ));
                                    }
                                    Err(e) => {
                                        // Wyczysc meeting_id bo join sie nie udal
                                        let client = router_client_handle.lock().await;
                                        if let Some(ref c) = *client {
                                            c.clear_meeting_id();
                                        }
                                        let _ = response_tx.send(format!("BLAD: {}", e));
                                    }
                                }
                            }
                            Err(e) => {
                                let client = router_client_handle.lock().await;
                                if let Some(ref c) = *client {
                                    c.clear_meeting_id();
                                }
                                let _ = response_tx.send(format!("BLAD: nie udalo sie uruchomic przegladarki: {}", e));
                            }
                        }
                    }
                    Some(quic_server::MeetingCommand::LeaveMeeting { response_tx }) => {
                        tracing::info!("Komenda QUIC: opuszczanie spotkania");
                        page = None;
                        if let Some(mut old_browser) = _chromium.take() {
                            let _ = old_browser.close().await;
                            let _ = old_browser.wait().await;
                        }
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

    let _ = shutdown_tx.send(true);
    tracing::info!("Sidecar meeting bot zakonczony");
    Ok(())
}
