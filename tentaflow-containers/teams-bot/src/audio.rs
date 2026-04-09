// =============================================================================
// Plik: audio.rs
// Opis: Przechwytywanie i odtwarzanie audio przez cpal (PulseAudio).
//       Odbiera PCM z wirtualnego urzadzenia audio i wysyla chunki do VAD.
// =============================================================================

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Czestotliwosc probkowania audio (16kHz — standard dla STT)
const SAMPLE_RATE: u32 = 16_000;

/// Liczba kanalow (mono)
const CHANNELS: u16 = 1;

/// Przechwytywanie audio z urzadzenia PulseAudio
pub struct AudioCapture {
    /// Kanal odbiorczy chunkow audio
    rx: mpsc::Receiver<Vec<i16>>,

    /// Strumien cpal — trzymamy referencje zeby nie zostal dropnienty
    _stream: cpal::Stream,
}

impl AudioCapture {
    /// Zwraca kolejny chunk audio (blokuje az do dostepnosci)
    pub async fn next_chunk(&mut self) -> Option<Vec<i16>> {
        self.rx.recv().await
    }
}

/// Uruchamia przechwytywanie audio z podanego urzadzenia PulseAudio
pub fn start_capture(device_name: Option<&str>, chunk_ms: u32) -> Result<AudioCapture> {
    let host = cpal::default_host();

    let device = match device_name {
        Some(name) => {
            // Szukamy urzadzenia po nazwie
            let devices = host.input_devices()
                .context("Nie mozna wylistowac urzadzen wejsciowych")?;

            let mut found = None;
            for d in devices {
                if let Ok(n) = d.name() {
                    if n.contains(name) {
                        found = Some(d);
                        break;
                    }
                }
            }

            found.with_context(|| format!("Nie znaleziono urzadzenia audio: {}", name))?
        }
        None => host
            .default_input_device()
            .context("Brak domyslnego urzadzenia wejsciowego")?,
    };

    let device_name_str = device.name().unwrap_or_else(|_| "nieznane".into());
    tracing::info!(device = %device_name_str, "Urzadzenie audio do przechwytywania");

    let config = cpal::StreamConfig {
        channels: CHANNELS,
        sample_rate: cpal::SampleRate(SAMPLE_RATE),
        buffer_size: cpal::BufferSize::Default,
    };

    // Bufor zbierajacy probki do momentu uzyskania pelnego chunka
    let samples_per_chunk = (SAMPLE_RATE * chunk_ms / 1000) as usize;
    let buffer: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::with_capacity(samples_per_chunk)));
    let buffer_clone = Arc::clone(&buffer);

    let (tx, rx) = mpsc::channel::<Vec<i16>>(32);

    let stream = device.build_input_stream(
        &config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            // Konwersja f32 -> i16
            let samples: Vec<i16> = data.iter().map(|&s| {
                (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
            }).collect();

            let mut buf = buffer_clone.lock().unwrap();
            buf.extend_from_slice(&samples);

            // Wyslij chunk gdy uzbieralo sie wystarczajaco probek
            while buf.len() >= samples_per_chunk {
                let chunk: Vec<i16> = buf.drain(..samples_per_chunk).collect();
                let _ = tx.try_send(chunk);
            }
        },
        move |err| {
            tracing::error!("Blad strumienia audio: {}", err);
        },
        None,
    ).context("Nie mozna utworzyc strumienia wejsciowego audio")?;

    stream.play().context("Nie mozna uruchomic strumienia audio")?;

    Ok(AudioCapture {
        rx,
        _stream: stream,
    })
}

/// Odtwarza dane PCM (i16, 16kHz, mono) na urzadzeniu wyjsciowym
pub fn play_audio(device_name: Option<&str>, pcm_data: &[i16]) -> Result<()> {
    let host = cpal::default_host();

    let device = match device_name {
        Some(name) => {
            let devices = host.output_devices()
                .context("Nie mozna wylistowac urzadzen wyjsciowych")?;

            let mut found = None;
            for d in devices {
                if let Ok(n) = d.name() {
                    if n.contains(name) {
                        found = Some(d);
                        break;
                    }
                }
            }

            found.with_context(|| format!("Nie znaleziono urzadzenia wyjsciowego: {}", name))?
        }
        None => host
            .default_output_device()
            .context("Brak domyslnego urzadzenia wyjsciowego")?,
    };

    let config = cpal::StreamConfig {
        channels: CHANNELS,
        sample_rate: cpal::SampleRate(SAMPLE_RATE),
        buffer_size: cpal::BufferSize::Default,
    };

    let data = pcm_data.to_vec();
    let data = Arc::new(Mutex::new(data.into_iter()));
    let data_clone = Arc::clone(&data);

    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

    let stream = device.build_output_stream(
        &config,
        move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut iter = data_clone.lock().unwrap();
            for sample in output.iter_mut() {
                match iter.next() {
                    Some(s) => *sample = s as f32 / i16::MAX as f32,
                    None => {
                        *sample = 0.0;
                        let _ = done_tx.send(());
                        return;
                    }
                }
            }
        },
        move |err| {
            tracing::error!("Blad odtwarzania audio: {}", err);
        },
        None,
    ).context("Nie mozna utworzyc strumienia wyjsciowego")?;

    stream.play().context("Nie mozna uruchomic odtwarzania")?;

    // Czekamy na zakonczenie odtwarzania
    let _ = done_rx.recv();

    Ok(())
}
