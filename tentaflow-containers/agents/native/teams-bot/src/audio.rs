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

/// Domyslny port mostu WebSocket. Docker uzywa 9999 (1 sesja per kontener,
/// brak kolizji). Native uruchamia wiele sesji w tym samym network namespace,
/// wiec port jest dynamiczny — `MeetingManager` alokuje go z `port_pool`
/// i przekazuje przez env `TENTAFLOW_BRIDGE_PORT`.
pub const DEFAULT_BRIDGE_PORT: u16 = 9999;

/// Zwraca port mostu WS — env `TENTAFLOW_BRIDGE_PORT` ma priorytet.
pub fn bridge_port() -> u16 {
    std::env::var("TENTAFLOW_BRIDGE_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(DEFAULT_BRIDGE_PORT)
}

/// Adres bind mostu WebSocket dla zadanego portu.
pub fn bridge_addr(port: u16) -> String {
    format!("127.0.0.1:{}", port)
}

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
pub async fn start_bridge() -> Result<(AudioCapture, AudioPlayback)> {
    start_bridge_on(bridge_port()).await
}

/// Wariant `start_bridge` z jawnym portem — uzywany w testach i przy
/// integracji z managerem ktory chce alokowac port samodzielnie.
pub async fn start_bridge_on(port: u16) -> Result<(AudioCapture, AudioPlayback)> {
    let (capture_tx, capture_rx) = mpsc::channel::<Vec<i16>>(CAPTURE_BUFFER);
    let playback_tx_slot: Arc<StdMutex<Option<mpsc::Sender<Vec<u8>>>>> = Arc::new(StdMutex::new(None));

    let addr = bridge_addr(port);
    let listener = TcpListener::bind(&addr).await
        .map_err(|e| anyhow::anyhow!("Nie udalo sie otworzyc {}: {}", addr, e))?;
    tracing::info!(addr = %addr, "Most audio WebSocket nasluchuje");

    let playback_slot_accept = Arc::clone(&playback_tx_slot);
    let bind_addr = addr.clone();

    // Task akceptujacy polaczenia od JS (Chromium injekcja)
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    tracing::info!(peer = %peer, "JS bridge polaczony");
                    let capture_tx_conn = capture_tx.clone();
                    let playback_slot_conn = Arc::clone(&playback_slot_accept);

                    tokio::spawn(async move {
                        if let Err(e) = handle_bridge_connection(
                            stream,
                            capture_tx_conn,
                            playback_slot_conn,
                        ).await {
                            tracing::warn!("Bridge polaczenie zakonczone: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Blad accept na {}: {}", bind_addr, e);
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

}
