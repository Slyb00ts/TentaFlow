// =============================================================================
// Plik: stt/audio.rs
// Opis: Dekoder audio — konwertuje WAV/MP3/OGG/surowe PCM na 16kHz mono f32,
//       format wymagany przez whisper.cpp.
// =============================================================================

use std::io::Cursor;

use anyhow::{bail, Context, Result};
use hound::WavReader;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Docelowa czestotliwosc probkowania dla whisper.cpp
const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Rozpoznany format audio
enum AudioFormat {
    Wav,
    Ogg,
    Mp3,
    RawPcm16,
}

/// Dekoduje dane audio do 16kHz mono f32 PCM.
/// Rozpoznaje format po magic bytes: WAV, OGG, MP3.
/// Nierozpoznane dane traktuje jako surowe PCM 16-bit LE 16kHz mono.
pub fn decode_to_pcm_f32(data: &[u8]) -> Result<Vec<f32>> {
    if data.len() < 4 {
        bail!("Dane audio zbyt krotkie ({} bajtow)", data.len());
    }

    let format = detect_format(data);

    match format {
        AudioFormat::Wav => decode_wav(data),
        AudioFormat::Mp3 | AudioFormat::Ogg => decode_symphonia(data, &format),
        AudioFormat::RawPcm16 => decode_raw_pcm16(data),
    }
}

/// Rozpoznaje format audio po magic bytes
fn detect_format(data: &[u8]) -> AudioFormat {
    // WAV: RIFF....WAVE
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WAVE" {
        return AudioFormat::Wav;
    }

    // OGG: OggS
    if data.len() >= 4 && &data[0..4] == b"OggS" {
        return AudioFormat::Ogg;
    }

    // MP3: ID3 tag
    if data.len() >= 3 && &data[0..3] == b"ID3" {
        return AudioFormat::Mp3;
    }

    // MP3: sync word (0xFF followed by 0xFA, 0xFB, 0xF2, 0xF3)
    if data.len() >= 2 && data[0] == 0xFF {
        match data[1] {
            0xFB | 0xFA | 0xF3 | 0xF2 => return AudioFormat::Mp3,
            _ => {}
        }
    }

    AudioFormat::RawPcm16
}

/// Dekoduje WAV przez hound
fn decode_wav(data: &[u8]) -> Result<Vec<f32>> {
    let reader = WavReader::new(Cursor::new(data)).context("Nie udalo sie otworzyc WAV")?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let sample_rate = spec.sample_rate;

    let samples_f32: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample;
            reader
                .into_samples::<i32>()
                .map(|s| {
                    let s = s.unwrap_or(0);
                    match bits {
                        16 => s as f32 / 32768.0,
                        24 => s as f32 / 8_388_608.0,
                        32 => s as f32 / 2_147_483_648.0,
                        _ => s as f32 / (1u32 << (bits - 1)) as f32,
                    }
                })
                .collect()
        }
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .map(|s| s.unwrap_or(0.0))
            .collect(),
    };

    let mono = to_mono(&samples_f32, channels);
    let resampled = if sample_rate != TARGET_SAMPLE_RATE {
        resample_linear(&mono, sample_rate, TARGET_SAMPLE_RATE)
    } else {
        mono
    };

    Ok(resampled)
}

/// Dekoduje MP3/OGG przez symphonia
fn decode_symphonia(data: &[u8], format: &AudioFormat) -> Result<Vec<f32>> {
    let cursor = Cursor::new(data.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    let mut hint = Hint::new();
    match format {
        AudioFormat::Mp3 => hint.with_extension("mp3"),
        AudioFormat::Ogg => hint.with_extension("ogg"),
        _ => &mut hint,
    };

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("Nie udalo sie rozpoznac formatu audio")?;

    let mut format_reader = probed.format;

    let track = format_reader
        .default_track()
        .context("Brak sciezki audio w pliku")?;

    let sample_rate = track
        .codec_params
        .sample_rate
        .context("Brak informacji o sample rate")?;

    let channels = track
        .codec_params
        .channels
        .map(|ch| ch.count())
        .unwrap_or(1);

    let track_id = track.id;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("Nie udalo sie utworzyc dekodera audio")?;

    let mut all_samples: Vec<f32> = Vec::new();

    // Dekoduj wszystkie pakiety
    loop {
        let packet = match format_reader.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let spec = *decoded.spec();
        let duration = decoded.capacity();

        let mut sample_buf = SampleBuffer::<f32>::new(duration as u64, spec);
        sample_buf.copy_interleaved_ref(decoded);

        all_samples.extend_from_slice(sample_buf.samples());
    }

    if all_samples.is_empty() {
        bail!("Nie udalo sie zdekodowac zadnych sampli audio");
    }

    let mono = to_mono(&all_samples, channels);
    let resampled = if sample_rate != TARGET_SAMPLE_RATE {
        resample_linear(&mono, sample_rate, TARGET_SAMPLE_RATE)
    } else {
        mono
    };

    Ok(resampled)
}

/// Dekoduje surowe PCM 16-bit LE 16kHz mono
fn decode_raw_pcm16(data: &[u8]) -> Result<Vec<f32>> {
    if data.len() % 2 != 0 {
        bail!(
            "Dane PCM maja nieparzysty rozmiar ({} bajtow), oczekiwano wielokrotnosci 2",
            data.len()
        );
    }

    let samples: Vec<f32> = data
        .chunks_exact(2)
        .map(|chunk| {
            let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
            sample as f32 / 32768.0
        })
        .collect();

    Ok(samples)
}

/// Konwertuje wielokanalowe audio do mono (srednia kanalow)
fn to_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }

    samples
        .chunks_exact(channels)
        .map(|frame| {
            let sum: f32 = frame.iter().sum();
            sum / channels as f32
        })
        .collect()
}

/// Resampling liniowa interpolacja z `from_rate` do `to_rate`
pub fn resample_linear(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || samples.is_empty() {
        return samples.to_vec();
    }

    let ratio = from_rate as f64 / to_rate as f64;
    let output_len = (samples.len() as f64 / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(output_len);

    for i in 0..output_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = src_pos - idx as f64;

        let sample = if idx + 1 < samples.len() {
            samples[idx] as f64 * (1.0 - frac) + samples[idx + 1] as f64 * frac
        } else {
            samples[samples.len() - 1] as f64
        };

        output.push(sample as f32);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_mono_stereo() {
        let stereo = vec![1.0, 0.0, 0.5, 0.5, 0.0, 1.0];
        let mono = to_mono(&stereo, 2);
        assert_eq!(mono, vec![0.5, 0.5, 0.5]);
    }

    #[test]
    fn test_to_mono_passthrough() {
        let mono_in = vec![0.1, 0.2, 0.3];
        let mono_out = to_mono(&mono_in, 1);
        assert_eq!(mono_out, mono_in);
    }

    #[test]
    fn test_resample_linear_same_rate() {
        let samples = vec![1.0, 2.0, 3.0];
        let out = resample_linear(&samples, 16000, 16000);
        assert_eq!(out, samples);
    }

    #[test]
    fn test_resample_linear_downsample() {
        // 32kHz -> 16kHz: powinno zmniejszyc liczbe sampli o polowe
        let samples: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let out = resample_linear(&samples, 32000, 16000);
        assert_eq!(out.len(), 50);
    }

    #[test]
    fn test_resample_linear_upsample() {
        // 8kHz -> 16kHz: powinno podwoic liczbe sampli
        let samples: Vec<f32> = (0..50).map(|i| i as f32).collect();
        let out = resample_linear(&samples, 8000, 16000);
        assert_eq!(out.len(), 100);
    }

    #[test]
    fn test_decode_raw_pcm16() {
        // Dwa sample: 0 i 32767 (max i16)
        let data: Vec<u8> = vec![0x00, 0x00, 0xFF, 0x7F];
        let out = decode_raw_pcm16(&data).unwrap();
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.0).abs() < f32::EPSILON);
        assert!((out[1] - (32767.0 / 32768.0)).abs() < 0.001);
    }

    #[test]
    fn test_detect_format_wav() {
        let mut data = vec![0u8; 12];
        data[0..4].copy_from_slice(b"RIFF");
        data[8..12].copy_from_slice(b"WAVE");
        assert!(matches!(detect_format(&data), AudioFormat::Wav));
    }

    #[test]
    fn test_detect_format_ogg() {
        let data = b"OggS\x00\x00\x00\x00";
        assert!(matches!(detect_format(data), AudioFormat::Ogg));
    }

    #[test]
    fn test_detect_format_mp3_id3() {
        let data = b"ID3\x04\x00\x00";
        assert!(matches!(detect_format(data), AudioFormat::Mp3));
    }

    #[test]
    fn test_detect_format_mp3_sync() {
        let data = vec![0xFF, 0xFB, 0x00, 0x00];
        assert!(matches!(detect_format(&data), AudioFormat::Mp3));
    }

    #[test]
    fn test_detect_format_raw_fallback() {
        let data = vec![0x00, 0x01, 0x02, 0x03];
        assert!(matches!(detect_format(&data), AudioFormat::RawPcm16));
    }

    #[test]
    fn test_decode_short_raw_pcm() {
        // 4 bajty = 2 sample PCM 16-bit
        let data = vec![0x00, 0x00, 0xFF, 0x7F];
        let result = decode_to_pcm_f32(&data).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_decode_too_short_error() {
        let data = vec![0x00];
        // 1 bajt — nieparzysty, ale fallback do raw PCM ktory wymaga parzystej dlugosci
        // Ale najpierw sprawdzi dlugosc < 4 — blad
        assert!(decode_to_pcm_f32(&data).is_err());
    }
}
