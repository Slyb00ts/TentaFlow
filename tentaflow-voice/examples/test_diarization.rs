// =============================================================================
// Plik: test_diarization.rs
// Opis: Test diarization — Silero VAD + WeSpeaker + online clustering.
//       Segmentuje audio, ekstrahuje embeddingi, przypisuje etykiety SPEAKER_XX
//       na podstawie cosine similarity do istniejacych centroidow.
// =============================================================================

use tentaflow_voice::{cosine_similarity, SileroVadStreaming, WeSpeaker};

const VAD_THRESHOLD: f32 = 0.5;
const MIN_SEGMENT_SAMPLES: usize = 16000 / 2; // 0.5s
const MIN_SILENCE_SAMPLES: usize = 16000 * 3 / 10; // 0.3s hysteresis
const SPEAKER_SIM_THRESHOLD: f32 = 0.45;
const MIN_RMS_FOR_EMBEDDING: f32 = 0.05; // odrzuc ciche okna (niewiarygodne embeddingi)

fn main() -> anyhow::Result<()> {
    let wav_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/sample_voices.wav".to_string());
    let vad_path = "/home/critix/repos/rust/TentaFlow/models/diarization/silero_vad.onnx";
    let embedding_path = "/home/critix/repos/rust/TentaFlow/models/diarization/embedding.onnx";

    println!("Ladowanie modeli...");
    let mut vad = SileroVadStreaming::from_file(vad_path).map_err(|e| anyhow::anyhow!("{}", e))?;
    let wespeaker = WeSpeaker::from_file(embedding_path).map_err(|e| anyhow::anyhow!("{}", e))?;
    println!("Modele zaladowane.\n");

    println!("Wczytywanie audio: {}", wav_path);
    let samples_i16 = read_wav_s16_mono_16k(&wav_path)?;
    let samples: Vec<f32> = samples_i16.iter().map(|&s| s as f32 / 32768.0).collect();
    let total_s = samples.len() as f32 / 16000.0;
    println!("Audio: {} probek, {:.2}s\n", samples.len(), total_s);

    // 1. Silero VAD — hop 512 samples, zbieraj segmenty mowy z histereza
    let chunk = 512;
    let mut vad_probs: Vec<f32> = Vec::new();
    for start in (0..samples.len()).step_by(chunk) {
        let end = (start + chunk).min(samples.len());
        let mut buf = [0.0_f32; 512];
        for (i, v) in samples[start..end].iter().enumerate() {
            buf[i] = *v;
        }
        let p = vad.predict(&buf).map_err(|e| anyhow::anyhow!("{}", e))?;
        vad_probs.push(p);
    }
    println!("VAD: {} chunkow po 32ms, sredni prob={:.3}",
        vad_probs.len(),
        vad_probs.iter().sum::<f32>() / vad_probs.len() as f32);

    // 2. Segmentacja: znajdz ciagle fragmenty gdzie VAD > threshold
    let segments = segment_speech(&vad_probs, chunk, samples.len());
    println!("Wykryto {} segmentow mowy:\n", segments.len());

    // 3. Dla kazdego segmentu: extract embedding + clustering
    let mut speakers: Vec<Speaker> = Vec::new();
    for (idx, seg) in segments.iter().enumerate() {
        if seg.end - seg.start < MIN_SEGMENT_SAMPLES {
            println!("  [segment {} pominiety — za krotki {:.2}s]",
                idx,
                (seg.end - seg.start) as f32 / 16000.0);
            continue;
        }
        let audio = &samples[seg.start..seg.end];
        let embedding = wespeaker.extract(audio).map_err(|e| anyhow::anyhow!("{}", e))?;

        let (speaker_id, sim) = assign_speaker(&mut speakers, &embedding);
        let start_s = seg.start as f32 / 16000.0;
        let end_s = seg.end as f32 / 16000.0;
        println!(
            "  SPEAKER_{:02}  [{:5.2}s - {:5.2}s] ({:4.2}s)  sim_to_centroid={:.3}",
            speaker_id, start_s, end_s, end_s - start_s, sim
        );
    }

    println!("\nPodsumowanie: {} unikalnych mowcow", speakers.len());
    for (i, sp) in speakers.iter().enumerate() {
        println!("  SPEAKER_{:02}: {} segmentow, L2 centroid={:.3}",
            i, sp.segment_count,
            sp.centroid.iter().map(|x| x * x).sum::<f32>().sqrt());
    }

    // 4. Macierz similarity miedzy centroidami (sanity check)
    if speakers.len() >= 2 {
        println!("\nMacierz podobienstwa centroidow:");
        for i in 0..speakers.len() {
            for j in 0..speakers.len() {
                let sim = cosine_similarity(&speakers[i].centroid, &speakers[j].centroid);
                print!("  {:6.3}", sim);
            }
            println!();
        }
    }

    // 5. Sliding-window diarization z filtrem energii (odrzuc cisze/bardzo ciche okna)
    //    Okno 2.5s, hop 0.5s. Threshold 0.45 (nizszy bo embeddingi w cichych regionach
    //    sa naturalnie mniej stabilne).
    println!("\n--- Sliding window diarization (okno 2.5s, hop 0.5s, RMS>={:.2}) ---",
        MIN_RMS_FOR_EMBEDDING);
    let win = 16000 * 5 / 2; // 2.5s
    let hop = 16000 / 2;     // 0.5s
    let mut sliding_speakers: Vec<Speaker> = Vec::new();
    let mut sliding_labels: Vec<(f32, Option<usize>, f32, f32)> = Vec::new();

    let mut start = 0usize;
    while start + win <= samples.len() {
        let audio = &samples[start..start + win];
        let rms = (audio.iter().map(|x| x * x).sum::<f32>() / audio.len() as f32).sqrt();
        let vad_start = start / chunk;
        let vad_end = ((start + win) / chunk).min(vad_probs.len());
        let avg_vad: f32 = vad_probs[vad_start..vad_end].iter().sum::<f32>()
            / (vad_end - vad_start).max(1) as f32;

        let t = start as f32 / 16000.0;
        if rms >= MIN_RMS_FOR_EMBEDDING && avg_vad >= VAD_THRESHOLD {
            let embedding = wespeaker.extract(audio).map_err(|e| anyhow::anyhow!("{}", e))?;
            let (id, sim) = assign_speaker(&mut sliding_speakers, &embedding);
            sliding_labels.push((t, Some(id), sim, rms));
        } else {
            sliding_labels.push((t, None, 0.0, rms));
        }
        start += hop;
    }

    for (t, id_opt, sim, rms) in &sliding_labels {
        match id_opt {
            Some(id) => println!("  {:5.2}s: SPEAKER_{:02} (sim={:.3}, rms={:.3})", t, id, sim, rms),
            None => println!("  {:5.2}s: [skip — silence/low-energy, rms={:.3}]", t, rms),
        }
    }

    println!("\nWykryto {} unikalnych mowcow", sliding_speakers.len());
    if sliding_speakers.len() >= 2 {
        println!("Macierz podobienstwa:");
        for i in 0..sliding_speakers.len() {
            for j in 0..sliding_speakers.len() {
                let sim = cosine_similarity(&sliding_speakers[i].centroid, &sliding_speakers[j].centroid);
                print!("  {:6.3}", sim);
            }
            println!();
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct Segment {
    start: usize,
    end: usize,
}

struct Speaker {
    centroid: Vec<f32>,
    segment_count: usize,
}

/// Zwraca ciagle segmenty gdzie VAD prob > threshold (z histereza ciszy).
fn segment_speech(probs: &[f32], chunk: usize, total_samples: usize) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut in_speech = false;
    let mut seg_start = 0;
    let mut silence_run = 0usize;

    for (i, &p) in probs.iter().enumerate() {
        let is_speech = p >= VAD_THRESHOLD;
        let pos = i * chunk;
        if is_speech {
            if !in_speech {
                seg_start = pos;
                in_speech = true;
            }
            silence_run = 0;
        } else if in_speech {
            silence_run += chunk;
            if silence_run >= MIN_SILENCE_SAMPLES {
                let end = (pos - silence_run + chunk).min(total_samples);
                segments.push(Segment { start: seg_start, end });
                in_speech = false;
                silence_run = 0;
            }
        }
    }
    if in_speech {
        segments.push(Segment { start: seg_start, end: total_samples });
    }
    segments
}

/// Online clustering: przypisz embedding do najblizszego centroidu (cos > threshold)
/// lub utworz nowego mowce. Zwraca (speaker_id, best_sim).
fn assign_speaker(speakers: &mut Vec<Speaker>, embedding: &[f32]) -> (usize, f32) {
    let mut best_idx = 0usize;
    let mut best_sim = -1.0_f32;
    for (i, sp) in speakers.iter().enumerate() {
        let s = cosine_similarity(&sp.centroid, embedding);
        if s > best_sim {
            best_sim = s;
            best_idx = i;
        }
    }

    if !speakers.is_empty() && best_sim >= SPEAKER_SIM_THRESHOLD {
        // Update centroidu (srednia krocząca)
        let sp = &mut speakers[best_idx];
        let n = sp.segment_count as f32;
        for (c, e) in sp.centroid.iter_mut().zip(embedding.iter()) {
            *c = (*c * n + *e) / (n + 1.0);
        }
        sp.segment_count += 1;
        (best_idx, best_sim)
    } else {
        let new_id = speakers.len();
        speakers.push(Speaker {
            centroid: embedding.to_vec(),
            segment_count: 1,
        });
        (new_id, best_sim)
    }
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
