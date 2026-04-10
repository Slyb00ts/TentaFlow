// =============================================================================
// Plik: vad.rs
// Opis: Detekcja aktywnosci glosowej (VAD). Detektor RMS z opcjonalnym
//       modelem Silero VAD (ONNX Runtime) dla wyzszej dokladnosci.
// =============================================================================

use anyhow::Result;
use ort::session::Session;
use ort::value::Tensor;

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

/// Stan wewnetrzny modelu Silero VAD (tensory h i c)
struct SileroState {
    h: Vec<f32>,
    c: Vec<f32>,
}

impl SileroState {
    fn new() -> Self {
        // Silero VAD wymaga tensorow [2, 1, 64] zainicjalizowanych zerami
        Self {
            h: vec![0.0f32; 2 * 1 * 64],
            c: vec![0.0f32; 2 * 1 * 64],
        }
    }
}

/// Detektor aktywnosci glosowej
pub struct VadDetector {
    /// Prog RMS powyzej ktorego uznajemy za mowe
    rms_threshold: f32,

    /// Liczba kolejnych chunkow ciszy
    silence_chunks: u32,

    /// Liczba chunkow ciszy wymagana do uznania konca wypowiedzi
    silence_chunks_threshold: u32,

    /// Czy poprzedni chunk byl mowa
    was_speech: bool,

    /// Sesja ONNX Runtime dla Silero VAD
    silero_session: Option<Session>,

    /// Stan wewnetrzny modelu Silero (tensory h/c)
    silero_state: Option<SileroState>,
}

impl VadDetector {
    /// Tworzy nowy detektor VAD
    ///
    /// `model_path` — sciezka do modelu ONNX Silero VAD (None = detektor RMS)
    ///
    /// `chunk_duration_ms` — czas trwania chunka w ms (do obliczenia progu ciszy)
    ///
    /// `silence_threshold_ms` — prog ciszy w ms po ktorym uznajemy koniec wypowiedzi
    ///
    /// `rms_threshold` — prog RMS powyzej ktorego uznajemy za mowe (uzywany gdy brak modelu ONNX)
    pub fn new(
        model_path: Option<&str>,
        chunk_duration_ms: u32,
        silence_threshold_ms: u32,
        rms_threshold: f32,
    ) -> Result<Self> {
        let silence_chunks_threshold = silence_threshold_ms / chunk_duration_ms;

        let (silero_session, silero_state) = if let Some(path) = model_path {
            match Session::builder()
                .and_then(|mut b| b.commit_from_file(path))
            {
                Ok(session) => {
                    tracing::info!(path, "Silero VAD ONNX zaladowany");
                    (Some(session), Some(SileroState::new()))
                }
                Err(e) => {
                    tracing::warn!("Nie udalo sie zaladowac Silero VAD: {} — uzywam RMS", e);
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        Ok(Self {
            rms_threshold,
            silence_chunks: 0,
            silence_chunks_threshold,
            was_speech: false,
            silero_session,
            silero_state,
        })
    }

    /// Przetwarza chunk audio i zwraca wynik VAD
    pub fn process_chunk(&mut self, samples: &[i16]) -> VadResult {
        let is_speech = if self.silero_session.is_some() {
            self.run_silero_inference(samples)
        } else {
            let rms = calculate_rms(samples);
            rms > self.rms_threshold
        };

        let result = if is_speech {
            self.silence_chunks = 0;
            self.was_speech = true;
            VadResult::Speech
        } else {
            self.silence_chunks += 1;

            if self.was_speech && self.silence_chunks >= self.silence_chunks_threshold {
                // Koniec wypowiedzi — wystarczajaco dlugo trwala cisza po mowie
                self.was_speech = false;
                self.silence_chunks = 0;
                VadResult::Transition
            } else {
                VadResult::Silence
            }
        };

        result
    }

    /// Resetuje stan detektora
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.silence_chunks = 0;
        self.was_speech = false;
        if self.silero_state.is_some() {
            self.silero_state = Some(SileroState::new());
        }
    }

    /// Uruchamia inferencje Silero VAD na probkach audio
    fn run_silero_inference(&mut self, samples: &[i16]) -> bool {
        // Borrow split — session i state to rozne pola, ale kompilator
        // wymaga jawnego destructuringu
        let (session, state) = match (&mut self.silero_session, &mut self.silero_state) {
            (Some(s), Some(st)) => (s, st),
            _ => return false,
        };

        // Konwersja i16 -> f32 (normalizacja do [-1.0, 1.0])
        let audio: Vec<f32> = samples.iter()
            .map(|&s| s as f32 / 32768.0)
            .collect();

        let audio_len = audio.len();

        // Przygotuj tensory wejsciowe
        let input = match Tensor::from_array(([1usize, audio_len], audio)) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("Blad tworzenia tensora audio: {}", e);
                return false;
            }
        };

        let h = match Tensor::from_array(([2usize, 1usize, 64usize], state.h.clone())) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("Blad tworzenia tensora h: {}", e);
                return false;
            }
        };

        let c = match Tensor::from_array(([2usize, 1usize, 64usize], state.c.clone())) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("Blad tworzenia tensora c: {}", e);
                return false;
            }
        };

        let sr = match Tensor::from_array(([1usize], vec![16000i64])) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("Blad tworzenia tensora sr: {}", e);
                return false;
            }
        };

        let inputs = ort::inputs![
            "input" => input,
            "h" => h,
            "c" => c,
            "sr" => sr,
        ];

        let outputs = match session.run(inputs) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("Blad inferencji Silero VAD: {}", e);
                return false;
            }
        };

        // Odczytaj prawdopodobienstwo mowy
        let probability = match outputs.get("output") {
            Some(val) => {
                match val.try_extract_tensor::<f32>() {
                    Ok((_shape, data)) => data[0],
                    Err(_) => return false,
                }
            }
            None => return false,
        };

        // Zaktualizuj stan h/c
        if let Some(hn) = outputs.get("hn") {
            if let Ok((_shape, data)) = hn.try_extract_tensor::<f32>() {
                state.h = data.to_vec();
            }
        }
        if let Some(cn) = outputs.get("cn") {
            if let Ok((_shape, data)) = cn.try_extract_tensor::<f32>() {
                state.c = data.to_vec();
            }
        }

        probability > 0.5
    }
}

/// Oblicza wartosc RMS (Root Mean Square) probek audio
fn calculate_rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_squares: f64 = samples.iter()
        .map(|&s| (s as f64) * (s as f64))
        .sum();

    (sum_squares / samples.len() as f64).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rms_silence() {
        let silence = vec![0i16; 8000];
        assert_eq!(calculate_rms(&silence), 0.0);
    }

    #[test]
    fn test_rms_signal() {
        // Sygnal o stalej amplitudzie 1000
        let signal = vec![1000i16; 8000];
        let rms = calculate_rms(&signal);
        assert!((rms - 1000.0).abs() < 1.0);
    }

    #[test]
    fn test_vad_speech_detection() {
        let mut vad = VadDetector::new(None, 500, 2000, 100.0).unwrap();

        // Glosny sygnal — mowa
        let loud = vec![5000i16; 8000];
        assert_eq!(vad.process_chunk(&loud), VadResult::Speech);

        // Cisza
        let quiet = vec![0i16; 8000];
        assert_eq!(vad.process_chunk(&quiet), VadResult::Silence);
    }

    #[test]
    fn test_vad_transition() {
        // 500ms chunk, 1000ms prog ciszy = 2 chunki ciszy na przejscie
        let mut vad = VadDetector::new(None, 500, 1000, 100.0).unwrap();

        let loud = vec![5000i16; 8000];
        let quiet = vec![0i16; 8000];

        // Mowa
        assert_eq!(vad.process_chunk(&loud), VadResult::Speech);

        // Pierwszy chunk ciszy
        assert_eq!(vad.process_chunk(&quiet), VadResult::Silence);

        // Drugi chunk ciszy — przejscie (koniec wypowiedzi)
        assert_eq!(vad.process_chunk(&quiet), VadResult::Transition);
    }

    #[test]
    fn test_rms_empty_samples() {
        // Pusta tablica probek — RMS = 0
        assert_eq!(calculate_rms(&[]), 0.0);
    }

    #[test]
    fn test_rms_single_sample() {
        // Jedna probka — RMS = wartosc bezwzgledna
        let rms = calculate_rms(&[500]);
        assert!((rms - 500.0).abs() < 1.0);
    }

    #[test]
    fn test_rms_negative_samples() {
        // Ujemne probki — RMS powinno byc takie samo jak dla dodatnich
        let positive = calculate_rms(&[1000, 1000, 1000]);
        let negative = calculate_rms(&[-1000, -1000, -1000]);
        assert!((positive - negative).abs() < 0.01);
    }

    #[test]
    fn test_vad_silence_without_prior_speech() {
        // Cisza bez poprzedzajacej mowy — nie powinno byc Transition
        let mut vad = VadDetector::new(None, 500, 1000, 100.0).unwrap();
        let quiet = vec![0i16; 8000];

        // Wiele chunkow ciszy bez uprzedniej mowy
        for _ in 0..10 {
            assert_eq!(vad.process_chunk(&quiet), VadResult::Silence);
        }
    }

    #[test]
    fn test_vad_multiple_transitions() {
        // Wiele cykli mowa->cisza->transition
        let mut vad = VadDetector::new(None, 500, 500, 100.0).unwrap();

        let loud = vec![5000i16; 8000];
        let quiet = vec![0i16; 8000];

        // Pierwszy cykl
        assert_eq!(vad.process_chunk(&loud), VadResult::Speech);
        assert_eq!(vad.process_chunk(&quiet), VadResult::Transition);

        // Po transition — stan zresetowany, cisza powinna byc Silence
        assert_eq!(vad.process_chunk(&quiet), VadResult::Silence);

        // Drugi cykl
        assert_eq!(vad.process_chunk(&loud), VadResult::Speech);
        assert_eq!(vad.process_chunk(&loud), VadResult::Speech);
        assert_eq!(vad.process_chunk(&quiet), VadResult::Transition);
    }

    #[test]
    fn test_vad_speech_resets_silence_counter() {
        // Mowa w trakcie odliczania ciszy resetuje licznik
        let mut vad = VadDetector::new(None, 500, 1500, 100.0).unwrap();

        let loud = vec![5000i16; 8000];
        let quiet = vec![0i16; 8000];

        // Mowa
        assert_eq!(vad.process_chunk(&loud), VadResult::Speech);

        // 2 chunki ciszy (prog = 3)
        assert_eq!(vad.process_chunk(&quiet), VadResult::Silence);
        assert_eq!(vad.process_chunk(&quiet), VadResult::Silence);

        // Mowa przerywa odliczanie
        assert_eq!(vad.process_chunk(&loud), VadResult::Speech);

        // Znowu cisza — licznik od nowa, 2 chunki to za malo
        assert_eq!(vad.process_chunk(&quiet), VadResult::Silence);
        assert_eq!(vad.process_chunk(&quiet), VadResult::Silence);

        // Trzeci chunk ciszy — teraz Transition
        assert_eq!(vad.process_chunk(&quiet), VadResult::Transition);
    }

    #[test]
    fn test_vad_reset() {
        // Reset stanu detektora
        let mut vad = VadDetector::new(None, 500, 500, 100.0).unwrap();

        let loud = vec![5000i16; 8000];
        let quiet = vec![0i16; 8000];

        // Ustaw stan — mowa
        assert_eq!(vad.process_chunk(&loud), VadResult::Speech);

        // Reset
        vad.reset();

        // Po resecie cisza nie powinna dac Transition (was_speech = false)
        assert_eq!(vad.process_chunk(&quiet), VadResult::Silence);
    }

    #[test]
    fn test_vad_threshold_boundary() {
        // Sygnal dokladnie na progu RMS (500.0) — nie powinien byc uznany za mowe
        // RMS = 500.0 -> is_speech = rms > 500.0 -> false
        let mut vad = VadDetector::new(None, 500, 1000, 500.0).unwrap();
        let at_threshold = vec![500i16; 8000];
        assert_eq!(vad.process_chunk(&at_threshold), VadResult::Silence);
    }
}
