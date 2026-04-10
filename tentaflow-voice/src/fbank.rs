// =============================================================================
// Plik: fbank.rs
// Opis: Kaldi-compatible Fbank features dla WeSpeaker — pure Rust implementacja
//       zgodna z torchaudio.compliance.kaldi.fbank(window_type='hamming').
//
//       Pipeline (per frame 25ms = 400 samples):
//       1. Pre-emphasis 0.97: y[t] = x[t] - 0.97 * x[t-1]
//       2. DC offset removal: y -= mean(y)
//       3. Hamming window
//       4. Zero-pad do 512 (next power of 2)
//       5. FFT → |X|^2 power spectrum
//       6. Mel filterbank (HTK mel scale, 80 bins)
//       7. log(max(mel, 1e-10))
//
//       Post-processing (zgodnie z wespeaker extract_deep_embedding.py):
//       - Mean subtraction across frames (NIE std normalization!)
// =============================================================================

use rustfft::{num_complex::Complex, FftPlanner};
use std::sync::OnceLock;

pub const SAMPLE_RATE: f32 = 16000.0;
pub const N_MELS: usize = 80;
pub const FRAME_LENGTH: usize = 400; // 25 ms @ 16kHz
pub const FRAME_SHIFT: usize = 160;  // 10 ms @ 16kHz
pub const N_FFT: usize = 512;        // next_power_of_2(400)
pub const PREEMPHASIS: f32 = 0.97;
pub const ENERGY_FLOOR: f32 = 1e-10;

/// Cached Hamming window i mel filterbank (budowane raz per runtime).
struct FbankCache {
    window: Vec<f32>,
    mel_fb: Vec<f32>, // [N_MELS, N_FFT/2+1]
}

static FBANK_CACHE: OnceLock<FbankCache> = OnceLock::new();

fn get_cache() -> &'static FbankCache {
    FBANK_CACHE.get_or_init(|| FbankCache {
        window: hamming_window(FRAME_LENGTH),
        mel_fb: build_mel_filterbank(N_FFT, SAMPLE_RATE, N_MELS),
    })
}

/// Hamming window: 0.54 - 0.46*cos(2*pi*n/(N-1))
fn hamming_window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            0.54 - 0.46 * (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos()
        })
        .collect()
}

/// HTK mel scale: mel = 1127 * ln(1 + f/700)
fn hz_to_mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * ((mel / 1127.0).exp() - 1.0)
}

/// Mel filterbank HTK-style (jak w kaldi z use_htk_mel_scale=true).
/// Zwraca flat array [n_mels * n_freqs] — wagi triangular filters.
fn build_mel_filterbank(n_fft: usize, sample_rate: f32, n_mels: usize) -> Vec<f32> {
    let n_freqs = n_fft / 2 + 1;
    let low_freq = 20.0_f32;
    let high_freq = sample_rate / 2.0;

    let mel_low = hz_to_mel(low_freq);
    let mel_high = hz_to_mel(high_freq);

    // n_mels + 2 equi-spaced mel points
    let mel_points: Vec<f32> = (0..n_mels + 2)
        .map(|i| mel_low + (mel_high - mel_low) * i as f32 / (n_mels + 1) as f32)
        .collect();
    let hz_points: Vec<f32> = mel_points.iter().map(|&m| mel_to_hz(m)).collect();

    // FFT bin index dla kazdej czestotliwosci
    let bin_from_hz = |hz: f32| -> f32 { hz * n_fft as f32 / sample_rate };
    let bins: Vec<f32> = hz_points.iter().map(|&hz| bin_from_hz(hz)).collect();

    let mut fb = vec![0.0_f32; n_mels * n_freqs];
    for m in 0..n_mels {
        let left = bins[m];
        let center = bins[m + 1];
        let right = bins[m + 2];
        for f in 0..n_freqs {
            let freq = f as f32;
            let weight = if freq < left || freq >= right {
                0.0
            } else if freq < center {
                (freq - left) / (center - left)
            } else {
                (right - freq) / (right - center)
            };
            fb[m * n_freqs + f] = weight;
        }
    }
    fb
}

/// Kaldi-compatible Fbank features: audio 16kHz mono f32 → flat [N_MELS, T]
/// Format wyjscia jest od razu gotowy dla Conv1d — [mel_bin, time_frame] row-major.
///
/// Per-thread scratch: planner FFT, fft_buffer, frame_buf, power_buf, mel_buf,
/// means. Wszystkie zyja w thread_local i rosna lazy. Hot path nie alokuje.
pub fn compute_fbank_into(samples: &[f32], out: &mut Vec<f32>) -> usize {
    if samples.len() < FRAME_LENGTH {
        out.clear();
        return 0;
    }

    let cache = get_cache();
    let n_freqs = N_FFT / 2 + 1;
    let n_frames = (samples.len() - FRAME_LENGTH) / FRAME_SHIFT + 1;

    // out bedzie [N_MELS, n_frames] — rosniemy w razie potrzeby
    let total = N_MELS * n_frames;
    if out.len() < total {
        out.resize(total, 0.0);
    }

    FBANK_SCRATCH.with(|cell| {
        let mut s = cell.borrow_mut();
        if s.fft.is_none() {
            let mut planner = FftPlanner::<f32>::new();
            s.fft = Some(planner.plan_fft_forward(N_FFT));
        }
        if s.fft_buffer.len() < N_FFT {
            s.fft_buffer.resize(N_FFT, Complex::new(0.0, 0.0));
        }
        if s.frame.len() < FRAME_LENGTH {
            s.frame.resize(FRAME_LENGTH, 0.0);
        }
        if s.power.len() < n_freqs {
            s.power.resize(n_freqs, 0.0);
        }

        // Destructure s raz — osobne &mut refs
        let FbankScratch { fft, fft_buffer, frame, power } = &mut *s;
        let fft = fft.as_ref().unwrap().clone();
        let frame = &mut frame[..FRAME_LENGTH];
        let fft_buffer = &mut fft_buffer[..N_FFT];
        let power = &mut power[..n_freqs];

        for frame_idx in 0..n_frames {
            let start = frame_idx * FRAME_SHIFT;
            let raw = &samples[start..start + FRAME_LENGTH];

            // 1+2+3. Pre-emphasis + DC + Hamming w jednej petli
            frame[0] = raw[0] - PREEMPHASIS * raw[0];
            let mut sum_f: f32 = frame[0];
            for t in 1..FRAME_LENGTH {
                let v = raw[t] - PREEMPHASIS * raw[t - 1];
                frame[t] = v;
                sum_f += v;
            }
            let mean = sum_f / FRAME_LENGTH as f32;
            let window = &cache.window[..];
            for t in 0..FRAME_LENGTH {
                frame[t] = (frame[t] - mean) * window[t];
            }

            // 4. Zero-pad do N_FFT i FFT
            for i in 0..FRAME_LENGTH {
                fft_buffer[i] = Complex::new(frame[i], 0.0);
            }
            for i in FRAME_LENGTH..N_FFT {
                fft_buffer[i] = Complex::new(0.0, 0.0);
            }
            fft.process(fft_buffer);

            // 5. Power spectrum
            for i in 0..n_freqs {
                let re = fft_buffer[i].re;
                let im = fft_buffer[i].im;
                power[i] = re * re + im * im;
            }

            // 6+7. Mel filterbank + log — piszemy od razu do out[mel, frame]
            for m in 0..N_MELS {
                let fb_row = &cache.mel_fb[m * n_freqs..(m + 1) * n_freqs];
                let mut acc = 0.0_f32;
                for f in 0..n_freqs {
                    acc += fb_row[f] * power[f];
                }
                out[m * n_frames + frame_idx] = acc.max(ENERGY_FLOOR).ln();
            }
        }
    });

    // 8. Mean subtraction per mel bin across frames (kluczowe dla WeSpeaker!)
    //    Mean liczony per wiersz mel (ciagle w pamieci → friendly do SIMD autovec)
    let inv_n = 1.0 / n_frames as f32;
    for m in 0..N_MELS {
        let row = &mut out[m * n_frames..(m + 1) * n_frames];
        let mut sum = 0.0_f32;
        for &v in row.iter() {
            sum += v;
        }
        let mean = sum * inv_n;
        for v in row.iter_mut() {
            *v -= mean;
        }
    }

    n_frames
}

/// Wariant z alokacja (backwards compat dla testow / bench).
pub fn compute_fbank(samples: &[f32]) -> Vec<Vec<f32>> {
    if samples.len() < FRAME_LENGTH {
        return Vec::new();
    }
    let mut flat = Vec::new();
    let n_frames = compute_fbank_into(samples, &mut flat);
    // Konwersja [N_MELS, n_frames] → Vec<[N_MELS]> × n_frames
    let mut out = Vec::with_capacity(n_frames);
    for t in 0..n_frames {
        let mut row = Vec::with_capacity(N_MELS);
        for m in 0..N_MELS {
            row.push(flat[m * n_frames + t]);
        }
        out.push(row);
    }
    out
}

struct FbankScratch {
    fft: Option<std::sync::Arc<dyn rustfft::Fft<f32>>>,
    fft_buffer: Vec<Complex<f32>>,
    frame: Vec<f32>,
    power: Vec<f32>,
}

impl FbankScratch {
    fn new() -> Self {
        Self {
            fft: None,
            fft_buffer: Vec::new(),
            frame: Vec::new(),
            power: Vec::new(),
        }
    }
}

thread_local! {
    static FBANK_SCRATCH: std::cell::RefCell<FbankScratch> = std::cell::RefCell::new(FbankScratch::new());
}

/// Konwertuje [num_frames][N_MELS] -> flat [N_MELS, num_frames] (transposed dla Conv1d)
pub fn fbank_to_conv_input(frames: &[Vec<f32>]) -> (Vec<f32>, usize) {
    if frames.is_empty() {
        return (Vec::new(), 0);
    }
    let num_frames = frames.len();
    let num_mels = frames[0].len();
    let mut out = vec![0.0_f32; num_mels * num_frames];
    for (t, frame) in frames.iter().enumerate() {
        for (m, &v) in frame.iter().enumerate() {
            out[m * num_frames + t] = v;
        }
    }
    (out, num_frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hamming_window_is_correct() {
        let w = hamming_window(4);
        assert!((w[0] - 0.08).abs() < 0.01);
        assert!((w[3] - 0.08).abs() < 0.01);
    }

    #[test]
    fn mel_to_hz_inverse() {
        let hz = 1000.0;
        let mel = hz_to_mel(hz);
        let back = mel_to_hz(mel);
        assert!((back - hz).abs() < 0.1);
    }

    #[test]
    fn fbank_output_length_matches_expected() {
        // 22848 samples, frame_len=400, frame_shift=160
        // n_frames = (22848 - 400) / 160 + 1 = 141
        let samples = vec![0.1_f32; 22848];
        let frames = compute_fbank(&samples);
        assert_eq!(frames.len(), 141);
        assert_eq!(frames[0].len(), N_MELS);
    }
}
