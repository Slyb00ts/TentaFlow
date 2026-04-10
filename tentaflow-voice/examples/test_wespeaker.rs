// Test WeSpeaker — załaduj model, ekstrahuj embedding z WAV, porównaj z reference ort.
use tentaflow_voice::{cosine_similarity, WeSpeaker};

fn main() -> anyhow::Result<()> {
    let wav_path = std::env::args().nth(1).unwrap_or_else(|| "/tmp/test_speech.wav".to_string());
    let model_path = "/home/critix/repos/rust/TentaFlow/models/diarization/embedding.onnx";

    println!("Ladowanie modelu WeSpeaker: {}", model_path);
    let start = std::time::Instant::now();
    let model = WeSpeaker::from_file(model_path).map_err(|e| anyhow::anyhow!("{}", e))?;
    println!("Zaladowany w {:?}\n", start.elapsed());

    println!("Wczytywanie audio: {}", wav_path);
    let samples = read_wav_s16_mono_16k(&wav_path)?;
    println!("Audio: {} probek, {:.2}s\n", samples.len(), samples.len() as f32 / 16000.0);

    let f32_samples: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();

    println!("Ekstrakcja embeddingu...");
    let start = std::time::Instant::now();
    let embedding = model.extract(&f32_samples).map_err(|e| anyhow::anyhow!("{}", e))?;
    let elapsed = start.elapsed();

    println!("Embedding 192-dim wyciagniety w {:?}", elapsed);
    println!("Pierwsze 10 wartosci: {:?}", &embedding[..10]);
    println!("Norma L2: {:.6}", embedding.iter().map(|x| x * x).sum::<f32>().sqrt());

    // Self-similarity (powinno być 1.0)
    let self_sim = cosine_similarity(&embedding, &embedding);
    println!("Self-similarity (oczekiwane 1.0): {:.6}", self_sim);

    // Stats
    let max = embedding.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let min = embedding.iter().cloned().fold(f32::INFINITY, f32::min);
    let mean = embedding.iter().sum::<f32>() / embedding.len() as f32;
    println!("Embedding stats: min={:.4}, max={:.4}, mean={:.4}", min, max, mean);

    // Zapisz pełen embedding do pliku dla porownania
    let mut bytes = Vec::with_capacity(embedding.len() * 4);
    for v in &embedding {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write("/tmp/rust_embedding.bin", &bytes)?;
    println!("Embedding zapisany do /tmp/rust_embedding.bin");

    println!("\nPerformance:");
    let audio_ms = (samples.len() as f64 / 16.0) as u64;
    let extract_ms = elapsed.as_millis();
    println!("  Audio length:  {} ms", audio_ms);
    println!("  Extract time:  {} ms", extract_ms);
    println!("  RTF:           {:.4}x", extract_ms as f64 / audio_ms as f64);

    Ok(())
}

fn read_wav_s16_mono_16k(path: &str) -> anyhow::Result<Vec<i16>> {
    let bytes = std::fs::read(path)?;
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        anyhow::bail!("Not a WAV");
    }
    let mut pos = 12;
    let mut data_offset = None;
    let mut data_size = 0;
    while pos + 8 <= bytes.len() {
        let cid = &bytes[pos..pos + 4];
        let csz = u32::from_le_bytes([bytes[pos+4], bytes[pos+5], bytes[pos+6], bytes[pos+7]]) as usize;
        pos += 8;
        if cid == b"data" {
            data_offset = Some(pos);
            data_size = csz;
            break;
        }
        pos += csz;
    }
    let off = data_offset.ok_or_else(|| anyhow::anyhow!("no data chunk"))?;
    let pcm = &bytes[off..off + data_size];
    Ok(pcm.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect())
}
