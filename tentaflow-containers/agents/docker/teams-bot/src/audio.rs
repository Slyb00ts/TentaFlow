// =============================================================================
// Plik: audio.rs
// Opis: Most audio miedzy wstrzyknietym JS w Chromium a Rust przez WebSocket
//       na 127.0.0.1:9999. Capture: ramki [0x02][PCM i16 16kHz mono] od JS.
//       Playback: ramki [0x01][PCM i16 16kHz mono] do JS (mic injection).
//       Zastepuje wczesniejsze podejscie z parec/pacat/PulseAudio monitor.
// =============================================================================

use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::net::TcpListener;
use std::sync::Mutex as StdMutex;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Adres mostu WebSocket — JS w Chromium laczy sie tutaj
pub const BRIDGE_ADDR: &str = "127.0.0.1:9999";

/// Rozmiar bufora kanalu capture (liczba chunkow)
const CAPTURE_BUFFER: usize = 64;

/// Pojedynczy wpis listy uczestnikow spotkania
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RosterEntry {
    pub name: String,
    #[serde(default)]
    pub status: String,
}

/// Odbiornik probek PCM z Chromium (monoton 16kHz i16, chunki ~256ms)
pub struct AudioCapture {
    rx: mpsc::Receiver<Vec<i16>>,
}

impl AudioCapture {
    /// Pobiera nastepny chunk audio od wstrzyknietego JS
    pub async fn next_chunk(&mut self) -> Option<Vec<i16>> {
        self.rx.recv().await
    }
}

/// Kanal odbioru aktualizacji rosteru (pelna lista przy kazdym tick 3s)
pub struct RosterReceiver {
    pub rx: mpsc::Receiver<Vec<RosterEntry>>,
}

/// Kanal odbioru zmian aktywnego mowcy (None = brak mowcy)
pub struct ActiveSpeakerReceiver {
    pub rx: mpsc::Receiver<Option<String>>,
}

/// Rozbija payload TTS na surowe bajty PCM i16 LE. Dla WAV parsuje chunki
/// (fmt, data) zamiast slepo pomijac 44 bajty — naglowek moze miec dodatkowe
/// chunki (LIST/INFO) i dane moga zaczynac sie pod innym offsetem.
/// Dla WAV wymaga PCM mono 16 kHz 16-bit, w przeciwnym razie zwraca blad.
pub fn parse_audio_payload(bytes: Vec<u8>) -> Result<Vec<u8>> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Ok(bytes);
    }
    let mut cursor = 12usize;
    let mut fmt_ok = false;
    let mut data_start: Option<usize> = None;
    while cursor + 8 <= bytes.len() {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_size = u32::from_le_bytes(
            bytes[cursor + 4..cursor + 8].try_into().map_err(|_| anyhow!("WAV chunk size read"))?,
        ) as usize;
        let body = cursor + 8;
        if chunk_id == b"fmt " && body + 16 <= bytes.len() {
            let format = u16::from_le_bytes(bytes[body..body + 2].try_into().unwrap());
            let channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap());
            let sample_rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
            let bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().unwrap());
            if format != 1 || channels != 1 || sample_rate != 16000 || bits != 16 {
                return Err(anyhow!(
                    "unsupported WAV: fmt={} ch={} sr={} bits={}",
                    format, channels, sample_rate, bits
                ));
            }
            fmt_ok = true;
        } else if chunk_id == b"data" {
            data_start = Some(body);
            break;
        }
        // chunk_size moze byc nieparzysty — spec wymaga pad byte
        cursor = body + chunk_size + (chunk_size & 1);
    }
    if !fmt_ok {
        return Err(anyhow!("WAV without fmt chunk"));
    }
    let start = data_start.ok_or_else(|| anyhow!("WAV without data chunk"))?;
    Ok(bytes[start..].to_vec())
}

/// Wysylka probek PCM do Chromium (mic injection bota).
/// Fire-and-forget: `send` jest sync i uzywa `try_send` — gdy bufor pelny,
/// ramka jest dropowana z ostrzezeniem (backpressure zamiast blokowania pipeline).
pub struct AudioPlayback {
    tx: Arc<StdMutex<Option<mpsc::Sender<Vec<u8>>>>>,
}

impl AudioPlayback {
    /// Wysyla dane PCM i16 16kHz mono do JS (ramka [0x01][payload]).
    /// Zwraca Err gdy JS bridge nie jest podlaczony; pelny bufor loguje warn i zwraca Ok.
    pub fn send(&self, pcm_data: Vec<u8>) -> Result<()> {
        let guard = self.tx.lock().map_err(|_| anyhow!("AudioPlayback mutex poisoned"))?;
        let Some(sender) = guard.as_ref() else {
            return Err(anyhow!("AudioPlayback: JS bridge niepodlaczony"));
        };
        match sender.try_send(pcm_data) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("playback buffer full, dropping PCM frame");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(anyhow!("AudioPlayback kanal zamkniety"))
            }
        }
    }
}

/// Uruchamia most WebSocket — nasluchuje na BRIDGE_ADDR, obsluguje jedno
/// polaczenie od wstrzyknietego JS (re-accept po rozlaczeniu).
/// Zwraca AudioCapture i AudioPlayback ktore maja ten sam interfejs co poprzednia
/// implementacja (zero zmian w main.rs pipeline).
pub async fn start_bridge() -> Result<(AudioCapture, AudioPlayback, RosterReceiver, ActiveSpeakerReceiver)> {
    let (capture_tx, capture_rx) = mpsc::channel::<Vec<i16>>(CAPTURE_BUFFER);
    let (roster_tx, roster_rx) = mpsc::channel::<Vec<RosterEntry>>(8);
    let (speaker_tx, speaker_rx) = mpsc::channel::<Option<String>>(32);
    let playback_tx_slot: Arc<StdMutex<Option<mpsc::Sender<Vec<u8>>>>> = Arc::new(StdMutex::new(None));

    let listener = TcpListener::bind(BRIDGE_ADDR).await
        .map_err(|e| anyhow::anyhow!("Nie udalo sie otworzyc {}: {}", BRIDGE_ADDR, e))?;
    tracing::info!(addr = BRIDGE_ADDR, "Most audio WebSocket nasluchuje");

    let playback_slot_accept = Arc::clone(&playback_tx_slot);

    // Task akceptujacy polaczenia od JS (Chromium injekcja)
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    tracing::info!(peer = %addr, "JS bridge polaczony");
                    let capture_tx_conn = capture_tx.clone();
                    let roster_tx_conn = roster_tx.clone();
                    let speaker_tx_conn = speaker_tx.clone();
                    let playback_slot_conn = Arc::clone(&playback_slot_accept);

                    tokio::spawn(async move {
                        if let Err(e) = handle_bridge_connection(
                            stream,
                            capture_tx_conn,
                            roster_tx_conn,
                            speaker_tx_conn,
                            playback_slot_conn,
                        ).await {
                            tracing::warn!("Bridge polaczenie zakonczone: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Blad accept na {}: {}", BRIDGE_ADDR, e);
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }
    });

    Ok((
        AudioCapture { rx: capture_rx },
        AudioPlayback { tx: playback_tx_slot },
        RosterReceiver { rx: roster_rx },
        ActiveSpeakerReceiver { rx: speaker_rx },
    ))
}

/// Obsluguje jedno polaczenie WebSocket od wstrzyknietego JS.
/// Rozdziela task odczytu (capture) i zapisu (playback).
async fn handle_bridge_connection(
    tcp: tokio::net::TcpStream,
    capture_tx: mpsc::Sender<Vec<i16>>,
    roster_tx: mpsc::Sender<Vec<RosterEntry>>,
    speaker_tx: mpsc::Sender<Option<String>>,
    playback_slot: Arc<StdMutex<Option<mpsc::Sender<Vec<u8>>>>>,
) -> Result<()> {
    let ws_stream = tokio_tungstenite::accept_async(tcp).await
        .map_err(|e| anyhow::anyhow!("Handshake WS blad: {}", e))?;
    let (mut ws_sink, mut ws_stream) = ws_stream.split();

    // Nowy kanal dla playback — wstaw do slotu
    let (pb_tx, mut pb_rx) = mpsc::channel::<Vec<u8>>(32);
    if let Ok(mut slot) = playback_slot.lock() {
        *slot = Some(pb_tx);
    }

    // Task wysylajacy ramki playback do JS
    let send_task = tokio::spawn(async move {
        while let Some(pcm_bytes) = pb_rx.recv().await {
            // Ramka: [0x01][PCM i16 little-endian]
            let mut frame = Vec::with_capacity(1 + pcm_bytes.len());
            frame.push(0x01);
            frame.extend_from_slice(&pcm_bytes);
            if ws_sink.send(Message::Binary(frame.into())).await.is_err() {
                break;
            }
        }
    });

    // Glowna petla — odbieraj ramki capture od JS
    let mut frame_count: u64 = 0;
    while let Some(msg) = ws_stream.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("WS stream blad: {}", e);
                break;
            }
        };
        match msg {
            Message::Binary(data) => {
                if data.len() < 2 {
                    continue;
                }
                let msg_type = data[0];
                match msg_type {
                    // 0x02 = PCM i16 capture z elementu <audio>/<video>
                    0x02 => {
                        let payload = &data[1..];
                        let samples = bytes_to_i16_samples(payload);
                        frame_count += 1;
                        // Log pierwsze 3 ramki + co 100-ta dla potwierdzenia przeplywu
                        if frame_count <= 3 || frame_count % 100 == 0 {
                            let rms = if samples.is_empty() {
                                0.0
                            } else {
                                (samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>()
                                    / samples.len() as f64).sqrt()
                            };
                            tracing::info!(frame = frame_count, samples = samples.len(), rms = format!("{:.1}", rms), "Capture PCM");
                        }
                        if capture_tx.send(samples).await.is_err() {
                            break;
                        }
                    }
                    // 0x03 = roster snapshot (JSON UTF-8)
                    0x03 => {
                        let payload = &data[1..];
                        match serde_json::from_slice::<Vec<RosterEntry>>(payload) {
                            Ok(list) => {
                                tracing::debug!(count = list.len(), "Roster update");
                                // try_send: gdy konsument nie nadaza, dropujemy stary
                                // snapshot — za chwile przyjdzie nowszy (polling 3s).
                                if let Err(mpsc::error::TrySendError::Closed(_)) = roster_tx.try_send(list) {
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Blad parsowania roster JSON: {}", e);
                            }
                        }
                    }
                    // 0x04 = active speaker change (nazwa UTF-8 albo pusty = brak)
                    0x04 => {
                        let payload = &data[1..];
                        let name = if payload.is_empty() {
                            None
                        } else {
                            Some(String::from_utf8_lossy(payload).to_string())
                        };
                        tracing::debug!(?name, "Active speaker change");
                        if let Err(mpsc::error::TrySendError::Closed(_)) = speaker_tx.try_send(name) {
                            break;
                        }
                    }
                    _ => {
                        tracing::debug!(msg_type, "Nieznany typ ramki od JS");
                    }
                }
            }
            Message::Close(_) => break,
            Message::Ping(p) => {
                let _ = p;
            }
            _ => {}
        }
    }

    // Wyczysc slot playback
    if let Ok(mut slot) = playback_slot.lock() {
        *slot = None;
    }
    send_task.abort();
    tracing::info!("JS bridge rozlaczony");
    Ok(())
}

/// Konwertuje surowe bajty LE na probki i16
pub fn bytes_to_i16_samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bytes_to_i16_samples() {
        let bytes = [0x00, 0x01, 0xFF, 0x7F, 0x00, 0x80];
        let samples = bytes_to_i16_samples(&bytes);
        assert_eq!(samples, vec![256, 32767, -32768]);
    }

    #[test]
    fn test_bytes_to_i16_samples_empty() {
        let samples = bytes_to_i16_samples(&[]);
        assert!(samples.is_empty());
    }

    #[test]
    fn test_bytes_to_i16_samples_odd() {
        let bytes = [0x00, 0x01, 0xFF];
        let samples = bytes_to_i16_samples(&bytes);
        assert_eq!(samples, vec![256]);
    }

    fn build_wav(sample_rate: u32, channels: u16, bits: u16, data: &[u8]) -> Vec<u8> {
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36u32 + data.len() as u32).to_le_bytes());
        w.extend_from_slice(b"WAVE");
        w.extend_from_slice(b"fmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&sample_rate.to_le_bytes());
        let byte_rate = sample_rate * channels as u32 * (bits as u32 / 8);
        w.extend_from_slice(&byte_rate.to_le_bytes());
        let block_align = channels * (bits / 8);
        w.extend_from_slice(&block_align.to_le_bytes());
        w.extend_from_slice(&bits.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&(data.len() as u32).to_le_bytes());
        w.extend_from_slice(data);
        w
    }

    #[test]
    fn parse_audio_payload_raw_passthrough() {
        let raw = vec![1, 2, 3, 4];
        assert_eq!(parse_audio_payload(raw.clone()).unwrap(), raw);
    }

    #[test]
    fn parse_audio_payload_valid_wav() {
        let pcm = vec![0xAA, 0xBB, 0xCC, 0xDD];
        let wav = build_wav(16000, 1, 16, &pcm);
        assert_eq!(parse_audio_payload(wav).unwrap(), pcm);
    }

    #[test]
    fn parse_audio_payload_rejects_bad_sample_rate() {
        let wav = build_wav(44100, 1, 16, &[0; 4]);
        assert!(parse_audio_payload(wav).is_err());
    }

    #[test]
    fn parse_audio_payload_rejects_stereo() {
        let wav = build_wav(16000, 2, 16, &[0; 4]);
        assert!(parse_audio_payload(wav).is_err());
    }
}
