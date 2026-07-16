// ===== File: audio.rs — WAV decoding to 16 kHz mono f32 for the STT frontend =====
// Accepts PCM i16/i24/i32 and IEEE f32 WAVs; multi-channel input is averaged
// to mono and non-16 kHz input is resampled. Downsampling first applies a
// windowed-sinc low-pass (7.2 kHz cutoff) so energy above the target Nyquist
// cannot alias into the mel bands; interpolation itself stays linear.

use std::io::{Cursor, Read};
use std::path::Path;

use forge_types::{ForgeError, Result};

use crate::mel::SAMPLE_RATE;

fn wav_err(e: hound::Error) -> ForgeError {
    ForgeError::Format(format!("wav: {e}"))
}

fn decode<R: Read>(reader: hound::WavReader<R>) -> Result<Vec<f32>> {
    let spec = reader.spec();
    let channels = spec.channels as usize;
    if channels == 0 {
        return Err(ForgeError::Format("wav: zero channels".into()));
    }

    let interleaved: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => reader
            .into_samples::<f32>()
            .collect::<std::result::Result<_, _>>()
            .map_err(wav_err)?,
        (hound::SampleFormat::Int, bits @ 16..=32) => {
            let scale = 1.0 / (1i64 << (bits - 1)) as f32;
            reader
                .into_samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<std::result::Result<_, _>>()
                .map_err(wav_err)?
        }
        (fmt, bits) => {
            return Err(ForgeError::Unsupported(format!(
                "wav: unsupported sample format {fmt:?} @ {bits} bits"
            )))
        }
    };

    let mono: Vec<f32> = interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();

    Ok(resample_linear(&mono, spec.sample_rate as usize))
}

/// Anti-aliasing FIR cutoff: below the 8 kHz target Nyquist with margin for
/// the filter's transition band.
const LOWPASS_CUTOFF_HZ: f64 = 7_200.0;
const LOWPASS_TAPS: usize = 63;

/// Hamming-windowed sinc low-pass at `cutoff_hz`, designed for `src_rate`.
fn lowpass_fir(src_rate: usize, cutoff_hz: f64) -> Vec<f32> {
    let fc = cutoff_hz / src_rate as f64; // normalized cutoff (cycles/sample)
    let mid = (LOWPASS_TAPS / 2) as f64;
    let mut taps = Vec::with_capacity(LOWPASS_TAPS);
    let mut sum = 0.0f64;
    for n in 0..LOWPASS_TAPS {
        let x = n as f64 - mid;
        let sinc = if x == 0.0 {
            2.0 * fc
        } else {
            (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
        };
        let window =
            0.54 - 0.46 * (2.0 * std::f64::consts::PI * n as f64 / (LOWPASS_TAPS - 1) as f64).cos();
        let t = sinc * window;
        sum += t;
        taps.push(t);
    }
    // Unity DC gain so speech loudness is preserved.
    taps.iter().map(|&t| (t / sum) as f32).collect()
}

/// Zero-phase-ish FIR convolution (edge samples use the zero-padded signal;
/// output is aligned to the filter's group delay).
fn convolve_fir(x: &[f32], taps: &[f32]) -> Vec<f32> {
    let half = taps.len() / 2;
    let n = x.len();
    let mut out = vec![0.0f32; n];
    for (i, o) in out.iter_mut().enumerate() {
        let mut acc = 0.0f32;
        for (k, &t) in taps.iter().enumerate() {
            let j = i as isize + k as isize - half as isize;
            if j >= 0 && (j as usize) < n {
                acc += t * x[j as usize];
            }
        }
        *o = acc;
    }
    out
}

fn resample_linear(mono: &[f32], src_rate: usize) -> Vec<f32> {
    if src_rate == SAMPLE_RATE || mono.is_empty() {
        return mono.to_vec();
    }
    // Downsampling folds everything above the target Nyquist back into the
    // audible band; band-limit first. Upsampling cannot alias.
    let filtered;
    let src: &[f32] = if src_rate > SAMPLE_RATE {
        filtered = convolve_fir(mono, &lowpass_fir(src_rate, LOWPASS_CUTOFF_HZ));
        &filtered
    } else {
        mono
    };
    let out_len = (src.len() as u64 * SAMPLE_RATE as u64 / src_rate as u64) as usize;
    let step = src_rate as f64 / SAMPLE_RATE as f64;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * step;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = src[idx.min(src.len() - 1)];
            let b = src[(idx + 1).min(src.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

/// Decode a WAV file into 16 kHz mono f32 samples.
pub fn load_wav(path: impl AsRef<Path>) -> Result<Vec<f32>> {
    decode(hound::WavReader::open(path.as_ref()).map_err(wav_err)?)
}

/// Decode in-memory WAV bytes into 16 kHz mono f32 samples (upload path).
pub fn decode_wav_bytes(bytes: &[u8]) -> Result<Vec<f32>> {
    decode(hound::WavReader::new(Cursor::new(bytes)).map_err(wav_err)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = hound::WavWriter::new(&mut buf, spec).unwrap();
            for &s in samples {
                w.write_sample(s).unwrap();
            }
            w.finalize().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn decodes_stereo_and_resamples() {
        // 8 kHz stereo constant signal → 16 kHz mono, doubled length,
        // channel-averaged amplitude.
        let samples: Vec<i16> = std::iter::repeat_n([8192i16, 24576i16], 800)
            .flatten()
            .collect();
        let out = decode_wav_bytes(&make_wav(8_000, 2, &samples)).unwrap();
        assert_eq!(out.len(), 1600);
        let expected = (8192.0 + 24576.0) / 2.0 / 32768.0;
        for &v in &out {
            assert!((v - expected).abs() < 1e-4, "{v} vs {expected}");
        }
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn downsampling_rejects_aliasing_tones() {
        // A 10 kHz tone at 48 kHz lies above the 8 kHz target Nyquist: naive
        // linear decimation would alias it to 6 kHz inside the mel band. The
        // anti-aliasing FIR must suppress it to near-silence, while a 3 kHz
        // tone passes with roughly unity gain.
        let sr = 48_000usize;
        let tone = |hz: f32| -> Vec<f32> {
            (0..sr)
                .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / sr as f32).sin())
                .collect()
        };
        let stop = resample_linear(&tone(10_000.0), sr);
        let pass = resample_linear(&tone(3_000.0), sr);
        assert_eq!(stop.len(), 16_000);
        let stop_rms = rms(&stop[1_000..15_000]);
        let pass_rms = rms(&pass[1_000..15_000]);
        // sin RMS is ~0.707; >40 dB attenuation in the stopband.
        assert!(stop_rms < 0.007, "aliasing energy leaked: rms {stop_rms}");
        assert!(
            (pass_rms - 0.707).abs() < 0.05,
            "passband distorted: rms {pass_rms}"
        );
    }

    #[test]
    fn passthrough_16k_mono() {
        let samples: Vec<i16> = (0..100).map(|i| (i * 300) as i16).collect();
        let out = decode_wav_bytes(&make_wav(16_000, 1, &samples)).unwrap();
        assert_eq!(out.len(), 100);
        assert!((out[1] - 300.0 / 32768.0).abs() < 1e-6);
    }
}
