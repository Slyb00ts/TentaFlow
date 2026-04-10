// =============================================================================
// Plik: examples/test_wav.rs
// Opis: Wczytuje WAV (16kHz mono s16le), puszcza przez SileroVad i wypisuje
//       prob per 32ms okno + ASCII wizualizacje + statystyki.
//
// Uzycie: cargo run --release --example test_wav -- <path_to.wav>
// =============================================================================

use tentaflow_voice::SileroVadStreaming;

const CHUNK: usize = 512; // 32 ms @ 16kHz
const THRESHOLD: f32 = 0.5;

fn main() -> anyhow::Result<()> {
    let wav_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/critix/test_mowa.wav".to_string());
    let model_path = "/home/critix/repos/rust/TentaFlow/models/diarization/silero_vad.onnx";

    // 1. Wczytaj WAV
    let samples = read_wav_s16_mono_16k(&wav_path)?;
    let duration_s = samples.len() as f32 / 16000.0;
    println!("WAV: {} probek, {:.2}s @ 16kHz mono", samples.len(), duration_s);

    // Konwersja i16 → f32
    let f32_samples: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();

    // Statystyki sygnalu
    let max_abs = f32_samples.iter().map(|x| x.abs()).fold(0.0_f32, f32::max);
    let rms: f32 = (f32_samples.iter().map(|x| x * x).sum::<f32>() / f32_samples.len() as f32).sqrt();
    println!("Signal peak: {:.4}, RMS: {:.4}", max_abs, rms);

    // 2. Zaladuj Silero VAD
    let mut vad = SileroVadStreaming::from_file(model_path)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    println!("Silero VAD pure Rust zaladowany\n");

    // 3. Przetworz wszystkie okna
    println!("{:>6} {:>8} {:>8} {:>30} {}", "t(ms)", "prob", "state", "bar", "");
    println!("{}", "-".repeat(80));

    let mut probs = Vec::new();
    let mut speech_count = 0;
    let mut first_speech_t = None;
    let mut last_speech_t = None;

    let start_time = std::time::Instant::now();

    for (i, window) in f32_samples.chunks_exact(CHUNK).enumerate() {
        let prob = vad.predict(window).map_err(|e| anyhow::anyhow!("{}", e))?;
        probs.push(prob);
        let t_ms = i * 32;

        let is_speech = prob > THRESHOLD;
        if is_speech {
            speech_count += 1;
            if first_speech_t.is_none() {
                first_speech_t = Some(t_ms);
            }
            last_speech_t = Some(t_ms);
        }

        // ASCII bar — 30 chars = 1.0 prob
        let bar_len = (prob * 30.0) as usize;
        let bar = "#".repeat(bar_len);
        let state = if is_speech { "SPEECH" } else { "silence" };

        // Loguj co 10 okien (co ~320ms) dla czytelnosci
        if i % 10 == 0 || is_speech {
            println!("{:>6} {:>8.4} {:>8} {:>30}", t_ms, prob, state, bar);
        }
    }

    let elapsed = start_time.elapsed();

    // 4. Statystyki
    println!("\n{}", "=".repeat(80));
    println!("PODSUMOWANIE:");
    let total_windows = probs.len();
    let avg_prob: f32 = probs.iter().sum::<f32>() / total_windows as f32;
    let max_prob: f32 = probs.iter().cloned().fold(0.0, f32::max);
    let min_prob: f32 = probs.iter().cloned().fold(1.0, f32::min);

    println!("  Okna total:           {}", total_windows);
    println!("  Prob min:             {:.4}", min_prob);
    println!("  Prob max:             {:.4}", max_prob);
    println!("  Prob sredni:          {:.4}", avg_prob);
    println!("  Okna speech (>{:.1}):   {} / {} ({:.1}%)",
        THRESHOLD, speech_count, total_windows,
        100.0 * speech_count as f32 / total_windows as f32);

    if let (Some(first), Some(last)) = (first_speech_t, last_speech_t) {
        println!("  Pierwsza mowa:        {} ms", first);
        println!("  Ostatnia mowa:        {} ms", last);
    } else {
        println!("  Brak wykrytej mowy (prob > {:.1})", THRESHOLD);
    }

    println!("\nWydajnosc:");
    let total_audio_ms = total_windows as f64 * 32.0;
    println!("  Audio:                {:.1} ms", total_audio_ms);
    println!("  CPU czas:             {:.2} ms", elapsed.as_secs_f64() * 1000.0);
    let per_chunk_us = elapsed.as_micros() / total_windows as u128;
    println!("  Per chunk:            {} us", per_chunk_us);
    println!("  Real-time factor:     {:.4}x", (per_chunk_us as f64) / 32_000.0);

    Ok(())
}

/// Prosty parser WAV (tylko s16le mono 16kHz PCM)
fn read_wav_s16_mono_16k(path: &str) -> anyhow::Result<Vec<i16>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 44 {
        anyhow::bail!("WAV za krotki");
    }

    // Walidacja header (minimum)
    if &bytes[0..4] != b"RIFF" {
        anyhow::bail!("Brak RIFF header");
    }
    if &bytes[8..12] != b"WAVE" {
        anyhow::bail!("Brak WAVE header");
    }

    // Znajdz chunk "fmt " i "data"
    let mut pos = 12;
    let mut data_offset = None;
    let mut data_size = 0;
    let mut sample_rate = 0;
    let mut channels = 0;
    let mut bits_per_sample = 0;

    while pos + 8 <= bytes.len() {
        let chunk_id = &bytes[pos..pos + 4];
        let chunk_size = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]]) as usize;
        pos += 8;

        match chunk_id {
            b"fmt " => {
                channels = u16::from_le_bytes([bytes[pos + 2], bytes[pos + 3]]);
                sample_rate = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]]);
                bits_per_sample = u16::from_le_bytes([bytes[pos + 14], bytes[pos + 15]]);
                pos += chunk_size;
            }
            b"data" => {
                data_offset = Some(pos);
                data_size = chunk_size;
                break;
            }
            _ => {
                pos += chunk_size;
            }
        }
    }

    if sample_rate != 16000 {
        anyhow::bail!("Oczekiwano 16000 Hz, dostano {}", sample_rate);
    }
    if channels != 1 {
        anyhow::bail!("Oczekiwano mono (1 ch), dostano {}", channels);
    }
    if bits_per_sample != 16 {
        anyhow::bail!("Oczekiwano 16 bit, dostano {}", bits_per_sample);
    }

    let data_start = data_offset.ok_or_else(|| anyhow::anyhow!("Brak 'data' chunk"))?;
    let pcm = &bytes[data_start..data_start + data_size];
    let mut samples = Vec::with_capacity(pcm.len() / 2);
    for chunk in pcm.chunks_exact(2) {
        samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
    }
    Ok(samples)
}
