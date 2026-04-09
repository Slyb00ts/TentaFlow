// =============================================================================
// Plik: audio.rs
// Opis: Przechwytywanie i odtwarzanie audio przez PulseAudio (libpulse-simple).
//       ALSA nie dziala w Docker bez /dev/snd — PulseAudio dziala natywnie.
// =============================================================================

use anyhow::Result;
use libpulse_binding as pulse;
use libpulse_simple_binding as psimple;
use tokio::sync::mpsc;

/// Przechwytywanie audio z PulseAudio — zwraca chunki PCM i16 przez kanal
pub struct AudioCapture {
    rx: mpsc::Receiver<Vec<i16>>,
    _handle: std::thread::JoinHandle<()>,
}

impl AudioCapture {
    /// Odbiera nastepny chunk audio (blokuje az bedzie dostepny)
    pub async fn next_chunk(&mut self) -> Option<Vec<i16>> {
        self.rx.recv().await
    }
}

/// Uruchamia przechwytywanie audio z domyslnego PulseAudio source.
/// Domyslne source to meeting_output.monitor (ustawione w pulseaudio.conf).
pub fn start_capture(device_name: Option<&str>, chunk_ms: u32) -> Result<AudioCapture> {
    let sample_rate = 16000u32;
    let samples_per_chunk = (sample_rate * chunk_ms as u32 / 1000) as usize;

    // PulseAudio monitor sinku — parec subprocess
    let device = Some(device_name.unwrap_or("speaker.monitor").to_string());
    let (tx, rx) = mpsc::channel::<Vec<i16>>(32);

    let handle = std::thread::spawn(move || {
        let source = device.as_deref().unwrap_or("speaker.monitor");

        // parec resampluje audio z monitora (float32le stereo 48kHz -> s16le mono 16kHz)
        let mut child = match std::process::Command::new("parec")
            .args(&["-d", source, "--format=s16le", "--channels=1", "--rate=16000", "--raw"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Nie udalo sie uruchomic parec: {:?}", e);
                return;
            }
        };

        tracing::info!(source = source, "parec uruchomiony — przechwytywanie audio");

        let stdout = child.stdout.take().expect("brak stdout parec");
        let mut reader = std::io::BufReader::new(stdout);
        let mut buf = vec![0u8; samples_per_chunk * 2];

        loop {
            use std::io::Read;
            match reader.read_exact(&mut buf) {
                Ok(()) => {
                    let samples: Vec<i16> = buf
                        .chunks_exact(2)
                        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                        .collect();

                    if tx.blocking_send(samples).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!("Blad odczytu parec: {:?}", e);
                    break;
                }
            }
        }
        let _ = child.kill();
    });

    tracing::info!(device = device_name.unwrap_or("default"), "Urzadzenie audio do przechwytywania");

    Ok(AudioCapture {
        rx,
        _handle: handle,
    })
}

/// Odtwarza audio PCM przez PulseAudio (do sinku tts_playback)
pub fn play_audio(device_name: Option<&str>, pcm_data: &[i16]) -> Result<()> {
    let spec = pulse::sample::Spec {
        format: pulse::sample::Format::S16le,
        channels: 1,
        rate: 16000,
    };

    let simple = psimple::Simple::new(
        None,
        "tentaflow-meeting",
        pulse::stream::Direction::Playback,
        device_name,
        "tts-playback",
        &spec,
        None,
        None,
    )
    .map_err(|e| anyhow::anyhow!("Nie udalo sie otworzyc PulseAudio sink: {:?}", e))?;

    let bytes: Vec<u8> = pcm_data
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();

    simple
        .write(&bytes)
        .map_err(|e| anyhow::anyhow!("Blad zapisu PulseAudio: {:?}", e))?;

    simple
        .drain()
        .map_err(|e| anyhow::anyhow!("Blad flush PulseAudio: {:?}", e))?;

    Ok(())
}
