// =============================================================================
// Plik: audio.rs
// Opis: Przechwytywanie i odtwarzanie audio przez parec/pacat.
//       Capture: parec z speaker.monitor (s16le mono 16kHz).
//       Playback: pacat do sink tts (s16le mono 16kHz).
//       PulseAudio module-loopback utrzymuje monitor aktywny.
// =============================================================================

use anyhow::Result;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Przechwytywanie audio z PulseAudio przez ffmpeg
pub struct AudioCapture {
    child: Option<std::process::Child>,
    _reader_handle: std::thread::JoinHandle<()>,
    rx: mpsc::Receiver<Vec<i16>>,
    shutdown: Arc<AtomicBool>,
}

impl AudioCapture {
    /// Pobiera nastepny chunk probek audio
    pub async fn next_chunk(&mut self) -> Option<Vec<i16>> {
        self.rx.recv().await
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(ref mut child) = self.child {
            let _ = child.kill();
        }
    }
}

/// Uruchamia przechwytywanie audio z PulseAudio przez ffmpeg
pub fn start_capture(chunk_ms: u32) -> Result<AudioCapture> {
    let rate = 16000u32;
    let byte_size = chunk_byte_size(chunk_ms, rate);
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_reader = Arc::clone(&shutdown);

    let mut child = spawn_ffmpeg_capture()?;
    let mut stdout = child.stdout.take()
        .ok_or_else(|| anyhow::anyhow!("Brak stdout z ffmpeg"))?;

    let (tx, rx) = mpsc::channel::<Vec<i16>>(32);

    let reader_handle = std::thread::spawn(move || {
        let mut buf = vec![0u8; byte_size];

        loop {
            match stdout.read_exact(&mut buf) {
                Ok(()) => {
                    let samples = bytes_to_i16_samples(&buf);
                    if tx.blocking_send(samples).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    if shutdown_reader.load(Ordering::SeqCst) {
                        break;
                    }
                    tracing::warn!("parec capture stdout zakonczony — respawn za 500ms");
                    std::thread::sleep(std::time::Duration::from_millis(500));

                    match spawn_ffmpeg_capture() {
                        Ok(mut new_child) => {
                            if let Some(new_stdout) = new_child.stdout.take() {
                                stdout = new_stdout;
                                tracing::info!("parec capture respawnowany");
                            } else {
                                tracing::error!("Brak stdout z nowego ffmpeg");
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::error!("Nie udalo sie respawnowac parec capture: {}", e);
                            break;
                        }
                    }
                }
            }
        }
    });

    tracing::info!(device = "speaker.monitor", chunk_ms, byte_size, "Audio capture uruchomiony");
    Ok(AudioCapture {
        child: Some(child),
        _reader_handle: reader_handle,
        rx,
        shutdown,
    })
}

/// Spawn ffmpeg do przechwytywania audio z PulseAudio
fn spawn_ffmpeg_capture() -> Result<std::process::Child> {
    // ffmpeg -f pulse -i speaker.monitor -ac 1 -ar 16000 -f s16le pipe:1
    Command::new("parec")
        .args([
            "--device=speaker.monitor",
            "--format=s16le",
            "--rate=16000",
            "--channels=1",
            "--raw",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Nie udalo sie uruchomic parec capture: {}", e))
}

/// Odtwarzanie audio przez ffmpeg do PulseAudio sink tts
pub struct AudioPlayback {
    child: Option<std::process::Child>,
    tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    _writer_handle: std::thread::JoinHandle<()>,
    shutdown: Arc<AtomicBool>,
}

impl AudioPlayback {
    /// Wysyla dane PCM do odtwarzania
    pub fn send(&self, pcm_data: Vec<u8>) {
        let _ = self.tx.send(pcm_data);
    }
}

impl Drop for AudioPlayback {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(ref mut child) = self.child {
            let _ = child.kill();
        }
    }
}

/// Uruchamia odtwarzanie audio do PulseAudio sink tts przez ffmpeg
pub fn start_playback() -> Result<AudioPlayback> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_writer = Arc::clone(&shutdown);

    let mut child = spawn_pacat_playback()?;
    let mut stdin = child.stdin.take()
        .ok_or_else(|| anyhow::anyhow!("Brak stdin z pacat playback"))?;

    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(4);

    let writer_handle = std::thread::spawn(move || {
        use std::io::Write;

        while let Ok(data) = rx.recv() {
            if let Err(_) = stdin.write_all(&data) {
                if shutdown_writer.load(Ordering::SeqCst) {
                    break;
                }
                tracing::warn!("pacat playback stdin blad — respawn za 500ms");
                std::thread::sleep(std::time::Duration::from_millis(500));

                match spawn_pacat_playback() {
                    Ok(mut new_child) => {
                        if let Some(new_stdin) = new_child.stdin.take() {
                            stdin = new_stdin;
                            tracing::info!("pacat playback respawnowany");
                        } else {
                            tracing::error!("Brak stdin z nowego pacat playback");
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::error!("Nie udalo sie respawnowac pacat playback: {}", e);
                        break;
                    }
                }
            }
        }
    });

    tracing::info!(device = "tts", "Audio playback uruchomiony");
    Ok(AudioPlayback {
        child: Some(child),
        tx,
        _writer_handle: writer_handle,
        shutdown,
    })
}

/// Spawn pacat do odtwarzania raw PCM do PulseAudio sink tts
fn spawn_pacat_playback() -> Result<std::process::Child> {
    Command::new("pacat")
        .args([
            "--device=tts",
            "--format=s16le",
            "--rate=16000",
            "--channels=1",
            "--raw",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Nie udalo sie uruchomic pacat playback: {}", e))
}

/// Konwertuje surowe bajty LE na probki i16
pub fn bytes_to_i16_samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Oblicza rozmiar chunka w bajtach
pub fn chunk_byte_size(chunk_ms: u32, sample_rate: u32) -> usize {
    (sample_rate * chunk_ms / 1000 * 2) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bytes_to_i16_samples() {
        // 0x0100 = 256 w LE, 0xFF7F = 32767 w LE, 0x0080 = -32768 w LE
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
    fn test_bytes_to_i16_samples_odd_bytes() {
        // Nieparzysta liczba bajtow — ostatni bajt obciety
        let bytes = [0x00, 0x01, 0xFF];
        let samples = bytes_to_i16_samples(&bytes);
        assert_eq!(samples, vec![256]);
    }

    #[test]
    fn test_chunk_byte_size() {
        // 500ms @ 16kHz = 16000 * 500 / 1000 * 2 = 16000 bajtow
        assert_eq!(chunk_byte_size(500, 16000), 16000);

        // 100ms @ 16kHz = 16000 * 100 / 1000 * 2 = 3200 bajtow
        assert_eq!(chunk_byte_size(100, 16000), 3200);
    }
}
