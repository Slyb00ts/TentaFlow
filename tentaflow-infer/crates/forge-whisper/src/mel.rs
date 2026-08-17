// ===== File: mel.rs — CPU log-mel spectrogram matching HF WhisperFeatureExtractor =====
// 16 kHz mono f32 → [80, 3000] log-mel. Semantics mirror the HF/OpenAI
// reference exactly: pad/truncate to 30 s, centered STFT (n_fft 400, hop 160,
// periodic Hann, reflect padding), power spectrum, slaney-scale + slaney-norm
// mel filterbank, log10 clamped to 1e-10, dynamic-range compression to
// (log_spec + 4) / 4 with an 8 dB floor below the global max.

use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;

pub const SAMPLE_RATE: usize = 16_000;
pub const N_FFT: usize = 400;
pub const HOP_LENGTH: usize = 160;
/// 30 s window the encoder was trained on.
pub const N_SAMPLES: usize = 30 * SAMPLE_RATE;
/// STFT frames fed to the encoder (the trailing frame is dropped like HF).
pub const N_FRAMES: usize = N_SAMPLES / HOP_LENGTH;

const N_FREQS: usize = N_FFT / 2 + 1;
const FMIN: f32 = 0.0;
const FMAX: f32 = 8_000.0;

/// Slaney-scale Hz→mel (librosa `hz_to_mel(htk=False)`).
fn hz_to_mel(hz: f32) -> f32 {
    const MIN_LOG_HZ: f32 = 1_000.0;
    const MIN_LOG_MEL: f32 = 15.0; // 1000 Hz / (200/3)
    let logstep = (6.4f32).ln() / 27.0;
    if hz < MIN_LOG_HZ {
        hz * 3.0 / 200.0
    } else {
        MIN_LOG_MEL + (hz / MIN_LOG_HZ).ln() / logstep
    }
}

/// Slaney-scale mel→Hz (librosa `mel_to_hz(htk=False)`).
fn mel_to_hz(mel: f32) -> f32 {
    const MIN_LOG_HZ: f32 = 1_000.0;
    const MIN_LOG_MEL: f32 = 15.0;
    let logstep = (6.4f32).ln() / 27.0;
    if mel < MIN_LOG_MEL {
        mel * 200.0 / 3.0
    } else {
        MIN_LOG_HZ * ((mel - MIN_LOG_MEL) * logstep).exp()
    }
}

/// Slaney-normalized triangular filterbank, [n_mels][N_FREQS] (librosa
/// `mel_filters` with norm="slaney", mel_scale="slaney"). `n_mels` follows
/// the checkpoint: 80 for base/small/medium/large-v2, 128 for large-v3/turbo.
fn mel_filterbank(n_mels: usize) -> Vec<Vec<f32>> {
    let mel_min = hz_to_mel(FMIN);
    let mel_max = hz_to_mel(FMAX);
    // n_mels + 2 band edges, linear in mel.
    let hz_pts: Vec<f32> = (0..n_mels + 2)
        .map(|i| mel_to_hz(mel_min + (mel_max - mel_min) * i as f32 / (n_mels + 1) as f32))
        .collect();
    let fft_freqs: Vec<f32> = (0..N_FREQS)
        .map(|f| f as f32 * SAMPLE_RATE as f32 / N_FFT as f32)
        .collect();

    let mut bank = vec![vec![0.0f32; N_FREQS]; n_mels];
    for (m, row) in bank.iter_mut().enumerate() {
        let (lo, mid, hi) = (hz_pts[m], hz_pts[m + 1], hz_pts[m + 2]);
        // Slaney normalization: peak scaled so each filter integrates to ~1.
        let enorm = 2.0 / (hi - lo);
        for (f, w) in row.iter_mut().enumerate() {
            let lower = (fft_freqs[f] - lo) / (mid - lo);
            let upper = (hi - fft_freqs[f]) / (hi - mid);
            *w = lower.min(upper).max(0.0) * enorm;
        }
    }
    bank
}

/// Compute the [n_mels * N_FRAMES] log-mel features (row-major: mel rows,
/// frame columns — the layout the encoder conv stem expects). Input is 16 kHz
/// mono; it is zero-padded or truncated to 30 s before the STFT.
pub fn log_mel_spectrogram(samples: &[f32], n_mels: usize) -> Vec<f32> {
    let mut audio = vec![0.0f32; N_SAMPLES];
    let n = samples.len().min(N_SAMPLES);
    audio[..n].copy_from_slice(&samples[..n]);

    // center=true reflect padding of n_fft/2 on both sides.
    let half = N_FFT / 2;
    let mut padded = vec![0.0f32; N_SAMPLES + N_FFT];
    padded[half..half + N_SAMPLES].copy_from_slice(&audio);
    for i in 1..=half {
        padded[half - i] = audio[i];
        padded[half + N_SAMPLES - 1 + i] = audio[N_SAMPLES - 1 - i];
    }

    // Periodic Hann window (torch.hann_window default).
    let window: Vec<f32> = (0..N_FFT)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / N_FFT as f32).cos()))
        .collect();

    let fft = FftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let mut buf = vec![Complex32::default(); N_FFT];
    let mut scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];

    // Power spectrum per frame; HF drops the last STFT frame, keeping N_FRAMES.
    let mut power = vec![0.0f32; N_FREQS * N_FRAMES];
    for t in 0..N_FRAMES {
        let start = t * HOP_LENGTH;
        for i in 0..N_FFT {
            buf[i] = Complex32::new(padded[start + i] * window[i], 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut scratch);
        for (f, b) in buf.iter().take(N_FREQS).enumerate() {
            power[f * N_FRAMES + t] = b.norm_sqr();
        }
    }

    let bank = mel_filterbank(n_mels);
    let mut log_spec = vec![0.0f32; n_mels * N_FRAMES];
    let mut global_max = f32::NEG_INFINITY;
    for (m, filt) in bank.iter().enumerate() {
        for t in 0..N_FRAMES {
            let mut acc = 0.0f32;
            for (f, &w) in filt.iter().enumerate() {
                if w > 0.0 {
                    acc += w * power[f * N_FRAMES + t];
                }
            }
            let v = acc.max(1e-10).log10();
            log_spec[m * N_FRAMES + t] = v;
            global_max = global_max.max(v);
        }
    }
    for v in &mut log_spec {
        *v = (v.max(global_max - 8.0) + 4.0) / 4.0;
    }
    log_spec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filterbank_matches_librosa_reference() {
        let bank = mel_filterbank(80);
        // Reference values from an independent float64 numpy port of the
        // librosa/HF slaney filterbank formula (mel_filter_bank with
        // norm="slaney", mel_scale="slaney", sr=16000, n_fft=400, fmax=8000).
        let checks = [
            (0usize, 1usize, 0.024862595f32),
            (1, 2, 0.022871772),
            (30, 30, 0.008962771),
            (40, 43, 0.0147355655),
            (60, 93, 0.006591093),
            (79, 195, 0.0022437952),
        ];
        for (m, f, expected) in checks {
            let got = bank[m][f];
            assert!(
                (got - expected).abs() < 2e-6,
                "filter[{m}][{f}] = {got}, expected {expected}"
            );
        }
        // Every filter must have positive mass (no dead mel bands).
        for (m, row) in bank.iter().enumerate() {
            assert!(row.iter().any(|&w| w > 0.0), "mel band {m} is empty");
        }
    }

    #[test]
    fn silence_produces_flat_floor() {
        let spec = log_mel_spectrogram(&vec![0.0; SAMPLE_RATE], 80);
        assert_eq!(spec.len(), 80 * N_FRAMES);
        // All-zero input: every bin clamps to log10(1e-10) = -10, then the
        // 8 dB floor and (x+4)/4 map everything to (-10 + 4) / 4 = -1.5.
        for &v in &spec {
            assert!((v + 1.5).abs() < 1e-6, "expected -1.5, got {v}");
        }
    }

    #[test]
    fn tone_is_deterministic_and_localized() {
        // 1 kHz tone for 1 s: energy concentrates in the same mel bands on
        // every run, and frames past the 1 s mark stay at the floor value.
        let tone: Vec<f32> = (0..SAMPLE_RATE)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / SAMPLE_RATE as f32).sin())
            .collect();
        let a = log_mel_spectrogram(&tone, 80);
        let b = log_mel_spectrogram(&tone, 80);
        assert_eq!(a, b);

        // Column energy at frame 50 (inside the tone) must exceed frame 2000
        // (silence) for the band containing 1 kHz.
        let m_1khz = (0..80)
            .max_by(|&x, &y| {
                a[x * N_FRAMES + 50]
                    .partial_cmp(&a[y * N_FRAMES + 50])
                    .unwrap()
            })
            .unwrap();
        assert!(a[m_1khz * N_FRAMES + 50] > a[m_1khz * N_FRAMES + 2000]);
    }

    #[test]
    fn supports_128_mel_bins() {
        // large-v3/turbo checkpoints use 128 mel bins; the filterbank and the
        // spectrogram must scale with the requested band count.
        let bank = mel_filterbank(128);
        assert_eq!(bank.len(), 128);
        for (m, row) in bank.iter().enumerate() {
            assert!(row.iter().any(|&w| w > 0.0), "mel band {m} is empty");
            assert!(row.iter().all(|w| w.is_finite()));
        }

        let tone: Vec<f32> = (0..SAMPLE_RATE)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SAMPLE_RATE as f32).sin())
            .collect();
        let spec = log_mel_spectrogram(&tone, 128);
        assert_eq!(spec.len(), 128 * N_FRAMES);
        assert!(spec.iter().all(|v| v.is_finite()));
        // The tone must register above the silence floor somewhere.
        assert!(spec.iter().any(|&v| v > -1.4));
    }
}
