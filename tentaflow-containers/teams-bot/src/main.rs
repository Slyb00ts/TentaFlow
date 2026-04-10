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

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

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
    // Inicjalizacja CryptoProvider dla rustls (wymagane przed uzyciem TLS)
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Blad instalacji CryptoProvider");

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

    // 2. Uruchom serwer QUIC — router laczy sie do kontenera
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let quic_config = quic_server::ContainerQuicConfig {
        port: config.quic_port,
        tls_cert: config.tls_cert.clone(),
        tls_key: config.tls_key.clone(),
        ..Default::default()
    };
    let quic = quic_server::MeetingQuicServer::new(quic_config);
    let transcript_tx = quic.transcript_sender();
    let router_client_handle = quic.router_client_handle();
    let mut command_rx = quic.command_receiver().await
        .expect("command_receiver powinien byc dostepny przy pierwszym wywolaniu");

    tokio::spawn(async move {
        if let Err(e) = quic.run(shutdown_rx).await {
            tracing::error!("Blad serwera QUIC: {}", e);
        }
    });
    tracing::info!(port = config.quic_port, "Serwer QUIC kontenera uruchomiony");

    // 3. Czekaj na polaczenie routera (potrzebujemy RouterClient do STT/TTS)
    tracing::info!("Czekam na polaczenie routera...");
    let router_client = loop {
        {
            let guard = router_client_handle.lock().await;
            if let Some(ref client) = *guard {
                break Arc::clone(client);
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    };
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

    // 5. Uruchom przechwytywanie audio
    let mut audio_capture = audio::start_capture(
        config.chunk_duration_ms,
    )?;
    tracing::info!("Przechwytywanie audio uruchomione");

    // 6. Inicjalizacja VAD
    let mut vad_detector = vad::VadDetector::new(
        config.vad_model_path.as_deref(),
        config.chunk_duration_ms,
        config.silence_threshold_ms,
        config.vad_rms_threshold,
    )?;

    // 7. Glowna petla: audio -> VAD -> STT (QUIC) -> streaming transcript -> TTS (QUIC)
    let mut speech_buffer: Vec<i16> = Vec::new();
    let stt_model = config.stt_model.as_deref().unwrap_or("teams-stt");
    let tts_model = config.tts_model.as_deref().unwrap_or("teams-tts");
    let tts_voice = config.tts_voice.as_deref().unwrap_or("alloy");

    tracing::info!("Pipeline audio uruchomiony (STT: {}, TTS: {})", stt_model, tts_model);

    let mut chunk_count: u64 = 0;

    loop {
        tokio::select! {
            chunk = audio_capture.next_chunk() => {
                let Some(chunk) = chunk else {
                    tracing::warn!("Strumien audio zakonczony");
                    break;
                };

                chunk_count += 1;

                // Debug: loguj RMS co 20 chunkow (~10s)
                if chunk_count % 20 == 0 {
                    let rms = (chunk.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / chunk.len() as f64).sqrt();
                    tracing::debug!(chunk_count, rms = format!("{:.1}", rms), "Audio RMS");
                }

                let vad_result = vad_detector.process_chunk(&chunk);

                match vad_result {
                    VadResult::Speech => {
                        speech_buffer.extend_from_slice(&chunk);
                    }
                    VadResult::Transition => {
                        speech_buffer.extend_from_slice(&chunk);

                        if !speech_buffer.is_empty() {
                            let speaker = if let Some(ref p) = page {
                                browser::get_active_speaker(p)
                                    .await
                                    .unwrap_or(None)
                                    .unwrap_or_else(|| "Nieznany".to_string())
                            } else {
                                "Nieznany".to_string()
                            };

                            let timestamp_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;

                            // Wyslij audio do STT przez router (reverse QUIC)
                            // Pobierz AKTUALNY RouterClient (moze sie zmienic po reconnect)
                            let current_client = router_client_handle.lock().await.clone();
                            let stt_result = match current_client {
                                Some(client) => client.transcribe(&speech_buffer, stt_model, None).await,
                                None => {
                                    tracing::warn!("Router nie polaczony — pomijam STT");
                                    Err(anyhow::anyhow!("brak RouterClient"))
                                }
                            };
                            match stt_result {
                                Ok(text) if !text.is_empty() => {
                                    tracing::info!(speaker = %speaker, "[{}]: {}", speaker, text);

                                    // Wyslij transkrypcje do streaming QUIC (router subskrybuje)
                                    let _ = transcript_tx.send((speaker, text, timestamp_ms));
                                }
                                Ok(_) => {
                                    tracing::debug!("STT zwrocil pusty tekst — pomijam");
                                }
                                Err(e) => {
                                    tracing::warn!("Blad STT: {} — pomijam segment", e);
                                }
                            }

                            speech_buffer.clear();
                        }
                    }
                    VadResult::Silence => {}
                }
            }
            cmd = command_rx.recv() => {
                match cmd {
                    Some(quic_server::MeetingCommand::JoinMeeting { meeting_url, response_tx }) => {
                        tracing::info!(meeting_url = %meeting_url, "Komenda QUIC: dolaczanie do spotkania");
                        match browser::launch_chromium(&config).await {
                            Ok(browser) => {
                                match browser::join_meeting(&browser, &meeting_url, &config).await {
                                    Ok(p) => {
                                        _chromium = Some(browser);
                                        page = Some(p);
                                        let _ = response_tx.send("OK: dolaczono do spotkania".to_string());
                                    }
                                    Err(e) => {
                                        let _ = response_tx.send(format!("BLAD: {}", e));
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = response_tx.send(format!("BLAD: nie udalo sie uruchomic przegladarki: {}", e));
                            }
                        }
                    }
                    Some(quic_server::MeetingCommand::LeaveMeeting { response_tx }) => {
                        tracing::info!("Komenda QUIC: opuszczanie spotkania");
                        page = None;
                        _chromium = None;
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
