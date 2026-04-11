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

    // 5. Uruchom most audio WebSocket (wstrzykniety JS w Chromium <-> Rust)
    // Zastepuje wczesniejsze parec/pacat — audio przechwytywane na poziomie
    // HTMLMediaElement w Chromium przez captureStream(), bez PulseAudio monitor.
    let (mut audio_capture, _audio_playback) = audio::start_bridge().await?;
    tracing::info!("Most audio WebSocket uruchomiony na 127.0.0.1:9999");

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
    let stt_model = config.stt_model.as_deref().unwrap_or("teams-stt");
    let tts_model = config.tts_model.as_deref().unwrap_or("teams-tts");
    let tts_voice = config.tts_voice.as_deref().unwrap_or("alloy");

    tracing::info!("Pipeline audio uruchomiony (STT: {}, TTS: {})", stt_model, tts_model);

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
                        model = stt_model,
                        "Wysylam segment do STT"
                    );

                    let current_client = router_client_handle.lock().await.clone();
                    let stt_result = match current_client {
                        Some(client) => {
                            match tokio::time::timeout(
                                std::time::Duration::from_secs(30),
                                client.transcribe(&speech_buffer, stt_model, Some("pl".to_string())),
                            ).await {
                                Ok(result) => result,
                                Err(_) => {
                                    tracing::warn!("STT timeout po 30s — router nie odpowiada");
                                    Err(anyhow::anyhow!("STT timeout"))
                                }
                            }
                        }
                        None => {
                            tracing::warn!("Router nie polaczony — pomijam STT");
                            Err(anyhow::anyhow!("brak RouterClient"))
                        }
                    };
                    match stt_result {
                        Ok(text) if !text.is_empty() => {
                            // Speaker label jest wywnioskowany po stronie routera z
                            // voice profiles + temp speaker tracker. Lokalny kanal
                            // `transcript_tx` dostarcza tylko fallback "Nieznany"
                            // bo nie wiemy po stronie bota jaki mowca byl — router
                            // wpisuje do swojego transcript_store.
                            tracing::info!(text = %text, "STT zwrocilo transkrypt");
                            let _ = transcript_tx.send(("Nieznany".to_string(), text, timestamp_ms));
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

                        // Wygeneruj unikalne meeting_id i ustaw na RouterClient
                        // — kazdy kolejny STT request bedzie je dopisywac do metadata
                        // ModelRequestu, router uzyje do klucza voice_temp_speakers
                        // i transcript_store.
                        let meeting_id = uuid::Uuid::new_v4().to_string();
                        tracing::info!(meeting_id = %meeting_id, "Nowe meeting_id wygenerowane");
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
