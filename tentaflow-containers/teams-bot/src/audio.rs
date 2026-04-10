// =============================================================================
// Plik: audio.rs
// Opis: Most audio miedzy wstrzyknietym JS w Chromium a Rust przez WebSocket
//       na 127.0.0.1:9999. Capture: ramki [0x02][PCM i16 16kHz mono] od JS.
//       Playback: ramki [0x01][PCM i16 16kHz mono] do JS (mic injection).
//       Zastepuje wczesniejsze podejscie z parec/pacat/PulseAudio monitor.
// =============================================================================

use anyhow::Result;
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;

/// Adres mostu WebSocket — JS w Chromium laczy sie tutaj
pub const BRIDGE_ADDR: &str = "127.0.0.1:9999";

/// Rozmiar bufora kanalu capture (liczba chunkow)
const CAPTURE_BUFFER: usize = 64;

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

/// Wysylka probek PCM do Chromium (mic injection bota)
pub struct AudioPlayback {
    tx: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
}

impl AudioPlayback {
    /// Wysyla dane PCM i16 16kHz mono do JS (ramka [0x01][payload])
    pub fn send(&self, pcm_data: Vec<u8>) {
        let tx_arc = Arc::clone(&self.tx);
        tokio::spawn(async move {
            let guard = tx_arc.lock().await;
            if let Some(ref sender) = *guard {
                let _ = sender.send(pcm_data).await;
            }
        });
    }
}

/// Uruchamia most WebSocket — nasluchuje na BRIDGE_ADDR, obsluguje jedno
/// polaczenie od wstrzyknietego JS (re-accept po rozlaczeniu).
/// Zwraca AudioCapture i AudioPlayback ktore maja ten sam interfejs co poprzednia
/// implementacja (zero zmian w main.rs pipeline).
pub async fn start_bridge() -> Result<(AudioCapture, AudioPlayback)> {
    let (capture_tx, capture_rx) = mpsc::channel::<Vec<i16>>(CAPTURE_BUFFER);
    let playback_tx_slot: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>> = Arc::new(Mutex::new(None));

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
                    let playback_slot_conn = Arc::clone(&playback_slot_accept);

                    tokio::spawn(async move {
                        if let Err(e) = handle_bridge_connection(stream, capture_tx_conn, playback_slot_conn).await {
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
    ))
}

/// Obsluguje jedno polaczenie WebSocket od wstrzyknietego JS.
/// Rozdziela task odczytu (capture) i zapisu (playback).
async fn handle_bridge_connection(
    tcp: tokio::net::TcpStream,
    capture_tx: mpsc::Sender<Vec<i16>>,
    playback_slot: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
) -> Result<()> {
    let ws_stream = tokio_tungstenite::accept_async(tcp).await
        .map_err(|e| anyhow::anyhow!("Handshake WS blad: {}", e))?;
    let (mut ws_sink, mut ws_stream) = ws_stream.split();

    // Nowy kanal dla playback — wstaw do slotu
    let (pb_tx, mut pb_rx) = mpsc::channel::<Vec<u8>>(32);
    {
        let mut slot = playback_slot.lock().await;
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
    {
        let mut slot = playback_slot.lock().await;
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
}
