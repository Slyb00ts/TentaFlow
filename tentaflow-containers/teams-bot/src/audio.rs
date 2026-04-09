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
    let sample_rate = 44100u32;
    let channels = 2u8;
    let samples_per_chunk = (sample_rate * chunk_ms as u32 / 1000) as usize * channels as usize;

    let spec = pulse::sample::Spec {
        format: pulse::sample::Format::F32le,
        channels,
        rate: sample_rate,
    };

    // Domyslne source: meeting_output.monitor (audio od uczestnikow spotkania)
    let device = Some(device_name.unwrap_or("meeting_output.monitor").to_string());
    let (tx, rx) = mpsc::channel::<Vec<i16>>(32);

    let handle = std::thread::spawn(move || {
        let source = device.as_deref();

        let simple = match psimple::Simple::new(
            None,
            "tentaflow-meeting",
            pulse::stream::Direction::Record,
            source,
            "meeting-capture",
            &spec,
            None,
            None,
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Nie udalo sie otworzyc PulseAudio source: {:?}", e);
                return;
            }
        };

        tracing::info!(
            source = source.unwrap_or("default (meeting_output.monitor)"),
            sample_rate = sample_rate,
            chunk_ms = chunk_ms,
            samples = samples_per_chunk,
            "PulseAudio capture uruchomiony"
        );

        // 4 bajty per sample (float32), 2 kanaly
        let mut buf = vec![0u8; samples_per_chunk * 4];

        loop {
            match simple.read(&mut buf) {
                Ok(()) => {
                    // float32le stereo -> i16 mono (srednia kanalow)
                    let floats: Vec<f32> = buf
                        .chunks_exact(4)
                        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                        .collect();

                    // Downmix stereo -> mono i konwersja float -> i16
                    let samples: Vec<i16> = floats
                        .chunks_exact(2)
                        .map(|pair| {
                            let mono = (pair[0] + pair[1]) * 0.5;
                            (mono * 32767.0).clamp(-32768.0, 32767.0) as i16
                        })
                        .collect();

                    if tx.blocking_send(samples).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!("Blad odczytu PulseAudio: {:?}", e);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
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
