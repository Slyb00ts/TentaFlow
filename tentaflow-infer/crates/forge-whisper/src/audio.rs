// ===== File: audio.rs — WAV decoding to 16 kHz mono f32 for the STT frontend =====
// Accepts PCM i16/i24/i32 and IEEE f32 WAVs; multi-channel input is averaged
// to mono and non-16 kHz input is linearly resampled. Linear interpolation is
// adequate here because Whisper's mel frontend low-passes at 8 kHz anyway.

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

fn resample_linear(mono: &[f32], src_rate: usize) -> Vec<f32> {
    if src_rate == SAMPLE_RATE || mono.is_empty() {
        return mono.to_vec();
    }
    let out_len = (mono.len() as u64 * SAMPLE_RATE as u64 / src_rate as u64) as usize;
    let step = src_rate as f64 / SAMPLE_RATE as f64;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * step;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = mono[idx.min(mono.len() - 1)];
            let b = mono[(idx + 1).min(mono.len() - 1)];
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

    #[test]
    fn passthrough_16k_mono() {
        let samples: Vec<i16> = (0..100).map(|i| (i * 300) as i16).collect();
        let out = decode_wav_bytes(&make_wav(16_000, 1, &samples)).unwrap();
        assert_eq!(out.len(), 100);
        assert!((out[1] - 300.0 / 32768.0).abs() < 1e-6);
    }
}
