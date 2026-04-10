use tentaflow_voice::fbank::{compute_fbank, fbank_to_conv_input};
fn main() -> anyhow::Result<()> {
    let bytes = std::fs::read("/tmp/test_speech.wav")?;
    let mut pos = 12;
    let mut data = vec![];
    while pos + 8 <= bytes.len() {
        let cid = &bytes[pos..pos+4];
        let csz = u32::from_le_bytes([bytes[pos+4], bytes[pos+5], bytes[pos+6], bytes[pos+7]]) as usize;
        pos += 8;
        if cid == b"data" {
            data = bytes[pos..pos+csz].to_vec();
            break;
        }
        pos += csz;
    }
    let samples: Vec<f32> = data.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect();
    println!("Samples: {}", samples.len());
    let fbank = compute_fbank(&samples);
    println!("Fbank frames: {}, mels per frame: {}", fbank.len(), fbank.first().map(|f| f.len()).unwrap_or(0));
    let (flat, t) = fbank_to_conv_input(&fbank);
    println!("Conv input shape: [{}, {}]", flat.len() / t.max(1), t);
    println!("First 10 of frame[0]: {:?}", &fbank[0][..10.min(fbank[0].len())]);
    Ok(())
}
