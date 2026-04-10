// =============================================================================
// Plik: vad.rs
// Opis: Voice Activity Detection — wrapper na tentaflow-voice::SileroVadStreaming
//       z dodatkowa logika sledzenia Speech/Transition/Silence dla pipeline STT.
//
// Silero VAD oczekuje blokow 512 probek (32ms @ 16kHz). Chunki od JS moga miec
// rozmiar 250ms (4000 probek) — dzielimy je na sub-windows i bierzemy max prob.
// =============================================================================

use anyhow::Result;
use tentaflow_voice::SileroVadStreaming;

const SILERO_WINDOW: usize = 512;

/// Wynik detekcji VAD dla jednego chunka audio
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadResult {
    /// Wykryto mowe
    Speech,
    /// Cisza
    Silence,
    /// Przejscie z mowy do ciszy (koniec wypowiedzi)
    Transition,
}

/// Detektor VAD — Silero ONNX (pure Rust, bez ort) + logika Speech/Transition
pub struct VadDetector {
    /// Silero VAD streaming inference (pure Rust forward pass)
    silero: Option<SileroVadStreaming>,

    /// Prog prawdopodobienstwa mowy (0.0-1.0)
    speech_threshold: f32,

    /// Fallback prog RMS gdy Silero niedostepny (raw i16)
    rms_threshold: f32,

    /// Liczba kolejnych chunkow ciszy
    silence_chunks: u32,

    /// Prog ciszy w chunkach do Transition
    silence_chunks_threshold: u32,

    /// Czy poprzedni chunk byl mowa
    was_speech: bool,
}

impl VadDetector {
    /// Tworzy nowy detektor VAD.
    ///
    /// `model_path` — sciezka do silero_vad.onnx. Jesli None albo blad ladowania,
    /// fallback na prosty detektor RMS.
    pub fn new(
        model_path: Option<&str>,
        chunk_duration_ms: u32,
        silence_threshold_ms: u32,
        rms_threshold: f32,
    ) -> Result<Self> {
        let silence_chunks_threshold = silence_threshold_ms / chunk_duration_ms;

        let silero = if let Some(path) = model_path {
            match SileroVadStreaming::from_file(path) {
                Ok(m) => {
                    tracing::info!(path, "Silero VAD (pure Rust) zaladowany");
                    Some(m)
                }
                Err(e) => {
                    tracing::warn!("Nie zaladowano Silero VAD: {} — fallback RMS", e);
                    None
                }
            }
        } else {
            tracing::warn!("Brak sciezki do modelu Silero VAD — uzywam RMS fallback");
            None
        };

        Ok(Self {
            silero,
            speech_threshold: 0.5,
            rms_threshold,
            silence_chunks: 0,
            silence_chunks_threshold,
            was_speech: false,
        })
    }

    /// Przetwarza chunk audio (raw i16 mono 16kHz) i zwraca wynik VAD.
    /// Dla chunkow wiekszych niz 512 probek bierzemy MAX probability
    /// po wszystkich sub-windows (512 sampli kazdy).
    pub fn process_chunk(&mut self, samples: &[i16]) -> VadResult {
        let is_speech = if self.silero.is_some() {
            self.run_silero(samples)
        } else {
            calculate_rms(samples) > self.rms_threshold
        };

        if is_speech {
            self.silence_chunks = 0;
            self.was_speech = true;
            VadResult::Speech
        } else {
            self.silence_chunks += 1;
            if self.was_speech && self.silence_chunks >= self.silence_chunks_threshold {
                self.was_speech = false;
                self.silence_chunks = 0;
                VadResult::Transition
            } else {
                VadResult::Silence
            }
        }
    }

    /// Resetuje stan detektora — nowy meeting/utwor
    pub fn reset(&mut self) {
        self.silence_chunks = 0;
        self.was_speech = false;
        if let Some(ref mut s) = self.silero {
            s.reset();
        }
    }

    /// Uruchamia Silero VAD na chunku. Dzieli chunk na sub-windows 512 sampli
    /// (wymagany rozmiar Silero) i zwraca true jesli max prob > threshold.
    fn run_silero(&mut self, samples: &[i16]) -> bool {
        let silero = match self.silero.as_mut() {
            Some(s) => s,
            None => return false,
        };

        // Konwersja i16 → f32 [-1, 1]
        let f32_samples: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();

        // Dziel na bloki 512 sampli — bierzemy max prob po wszystkich oknach
        let mut max_prob = 0.0_f32;
        let mut any_window_processed = false;

        for window in f32_samples.chunks_exact(SILERO_WINDOW) {
            match silero.predict(window) {
                Ok(prob) => {
                    any_window_processed = true;
                    if prob > max_prob {
                        max_prob = prob;
                    }
                }
                Err(e) => {
                    tracing::warn!("Silero predict blad: {}", e);
                    return false;
                }
            }
        }

        // Obsluga ogonka — jesli chunk nie jest wielokrotnoscia 512,
        // dopad zerami ostatnie okno i puszcz przez model.
        let tail_len = f32_samples.len() % SILERO_WINDOW;
        if tail_len > 0 {
            let mut padded = vec![0.0_f32; SILERO_WINDOW];
            let tail_start = f32_samples.len() - tail_len;
            padded[..tail_len].copy_from_slice(&f32_samples[tail_start..]);
            if let Ok(prob) = silero.predict(&padded) {
                any_window_processed = true;
                if prob > max_prob {
                    max_prob = prob;
                }
            }
        }

        any_window_processed && max_prob > self.speech_threshold
    }
}

/// Prosty detektor RMS (fallback gdy brak modelu Silero)
fn calculate_rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples
        .iter()
        .map(|&s| (s as f64).powi(2))
        .sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rms_silence() {
        assert_eq!(calculate_rms(&[0, 0, 0, 0]), 0.0);
    }

    #[test]
    fn test_rms_signal() {
        let samples: Vec<i16> = vec![100, -100, 100, -100];
        assert!((calculate_rms(&samples) - 100.0).abs() < 0.1);
    }

    #[test]
    fn test_rms_empty_samples() {
        assert_eq!(calculate_rms(&[]), 0.0);
    }

    #[test]
    fn test_rms_single_sample() {
        assert_eq!(calculate_rms(&[500]), 500.0);
    }

    #[test]
    fn test_rms_negative_samples() {
        let samples = vec![-1000_i16, -1000, -1000, -1000];
        assert!((calculate_rms(&samples) - 1000.0).abs() < 0.1);
    }

    #[test]
    fn test_vad_silence_without_prior_speech() {
        // Cisza bez wczesniejszej mowy → Silence (nie Transition)
        let mut vad = VadDetector::new(None, 500, 2000, 100.0).unwrap();
        let silence = vec![0_i16; 8000];
        assert_eq!(vad.process_chunk(&silence), VadResult::Silence);
        assert_eq!(vad.process_chunk(&silence), VadResult::Silence);
    }

    #[test]
    fn test_vad_speech_detection_rms() {
        // Sygnal RMS 500 (powyzej progu 100) → Speech
        let mut vad = VadDetector::new(None, 500, 2000, 100.0).unwrap();
        let loud: Vec<i16> = vec![500, -500, 500, -500].repeat(2000);
        assert_eq!(vad.process_chunk(&loud), VadResult::Speech);
    }

    #[test]
    fn test_vad_transition_after_silence() {
        // Mowa → cisza × 4 → Transition (silence_threshold_ms / chunk_duration_ms = 4)
        let mut vad = VadDetector::new(None, 500, 2000, 100.0).unwrap();
        let loud: Vec<i16> = vec![500, -500, 500, -500].repeat(2000);
        let silence = vec![0_i16; 8000];
        assert_eq!(vad.process_chunk(&loud), VadResult::Speech);
        assert_eq!(vad.process_chunk(&silence), VadResult::Silence);
        assert_eq!(vad.process_chunk(&silence), VadResult::Silence);
        assert_eq!(vad.process_chunk(&silence), VadResult::Silence);
        assert_eq!(vad.process_chunk(&silence), VadResult::Transition);
    }

    #[test]
    fn test_vad_reset() {
        let mut vad = VadDetector::new(None, 500, 2000, 100.0).unwrap();
        let loud: Vec<i16> = vec![500, -500].repeat(4000);
        vad.process_chunk(&loud);
        vad.reset();
        // Po reset, jedna cisza nie powinna byc Transition
        let silence = vec![0_i16; 8000];
        assert_eq!(vad.process_chunk(&silence), VadResult::Silence);
    }
}
