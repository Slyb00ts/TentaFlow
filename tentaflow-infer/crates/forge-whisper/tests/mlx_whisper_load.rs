// ===== File: mlx_whisper_load.rs — loading a real MLX Whisper checkpoint =====
//
// Runs the whole loader against `mlx-community/whisper-large-v3-turbo-4bit`
// on the CPU backend, so the path is exercised end to end without a GPU:
// OpenAI naming, OpenAI config keys, quantized triples, transposed convolution
// kernels and generated encoder positions.
//
// The dequantized weights are compared against values produced by MLX itself
// (fixture in forge-formats), because "the model loaded" is not evidence that
// it loaded the right numbers.

use std::path::PathBuf;

use forge_hal::cpu::CpuDevice;
use forge_whisper::flavour::{sinusoids, WhisperFlavour};
use forge_whisper::weights::WhisperWeights;
use half::f16;

fn checkpoint() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("HOME")?)
        .join("Library/Application Support/tentaflow/models/mlx-whisper")
        .join("mlx-community_whisper-large-v3-turbo-4bit");
    dir.join("model.safetensors").is_file().then_some(dir)
}

fn read_f16(device: &CpuDevice, buf: &forge_hal::DevBuffer, count: usize) -> Vec<f32> {
    use forge_hal::Device;
    let mut bytes = vec![0u8; count * 2];
    device.read(buf, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(2)
        .map(|c| f16::from_bits(u16::from_le_bytes(c.try_into().unwrap())).to_f32())
        .collect()
}

#[test]
fn loads_mlx_whisper_large_v3_turbo() {
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu MLX Whisper");
        return;
    };
    let device = CpuDevice::new();
    let w = WhisperWeights::load(&*device, &dir).expect("wczytanie checkpointu MLX Whisper");

    // Wymiary przetłumaczone ze schematu OpenAI na schemat HF.
    assert_eq!(w.config.d_model, 1280);
    assert_eq!(w.config.encoder_layers, 32);
    assert_eq!(w.config.decoder_layers, 4);
    assert_eq!(w.config.encoder_attention_heads, 20);
    assert_eq!(w.config.encoder_ffn_dim, 5120);
    assert_eq!(w.config.num_mel_bins, 128);
    assert_eq!(w.config.max_source_positions, 1500);
    assert_eq!(w.config.max_target_positions, 448);
    assert_eq!(w.config.vocab_size, 51866);
    // Identyfikatory tokenów są wyłącznie w generation_config.json.
    assert_eq!(w.config.decoder_start_token_id, 50258);
    assert_eq!(w.config.eos_token_id, 50257);

    assert_eq!(w.enc_layers.len(), 32);
    assert_eq!(w.dec_layers.len(), 4);

    // Pozycje enkodera są generowane, nie wczytywane.
    assert_eq!(w.enc_pos_host.len(), 1500 * 1280);
    let expected = sinusoids(1500, 1280).unwrap();
    assert_eq!(w.enc_pos_host, expected);
    for i in 0..640 {
        assert_eq!(w.enc_pos_host[i], 0.0, "sin(0) na pozycji 0, kanał {i}");
        assert_eq!(w.enc_pos_host[640 + i], 1.0, "cos(0) na pozycji 0");
    }
}

#[test]
fn dequantized_weights_match_the_mlx_oracle() {
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu MLX Whisper");
        return;
    };
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../forge-formats/tests/fixtures/mlx_affine_whisper.bin");
    let Ok(blob) = std::fs::read(&fixture) else {
        eprintln!("pomijam: brak fikstury {}", fixture.display());
        return;
    };

    let device = CpuDevice::new();
    let w = WhisperWeights::load(&*device, &dir).expect("wczytanie checkpointu");

    // Fikstura trzyma dwa pierwsze wiersze `encoder.blocks.0.mlp1`, policzone
    // przez `mx.dequantize`. Loader musi dać te same liczby po zawężeniu do f16.
    let (cols, want) = first_case_rows(&blob, "encoder.blocks.0.mlp1");
    let got = read_f16(&device, &w.enc_layers[0].fc1_w, want.len());
    assert_eq!(cols, 1280);
    assert_eq!(got.len(), want.len());

    let mut mismatches = 0;
    for (i, (g, v)) in got.iter().zip(&want).enumerate() {
        if f16::from_f32(*v).to_f32() != *g {
            mismatches += 1;
            assert!(
                mismatches < 5,
                "element {i}: loader dał {g}, MLX {v} (i {mismatches} innych)"
            );
        }
    }
    assert_eq!(mismatches, 0, "{mismatches} wartości różni się od MLX");
}

/// Wyciąga z fikstury oczekiwane wartości dla podanego tensora.
/// Format opisany w `tools/mlx-oracle/gen_fixtures.py`.
fn first_case_rows(blob: &[u8], want_name: &str) -> (usize, Vec<f32>) {
    let mut pos = 4usize;
    let u32_at = |p: &mut usize| {
        let v = u32::from_le_bytes(blob[*p..*p + 4].try_into().unwrap());
        *p += 4;
        v
    };
    assert_eq!(u32_at(&mut pos), 2, "wersja fikstury");
    let _group = u32_at(&mut pos);
    let _bits = u32_at(&mut pos);
    let count = u32_at(&mut pos);

    for _ in 0..count {
        let name_len = u32_at(&mut pos) as usize;
        let name = String::from_utf8(blob[pos..pos + name_len].to_vec()).unwrap();
        pos += name_len;
        let _rows = u32_at(&mut pos);
        let _packed_cols = u32_at(&mut pos);
        let cols = u32_at(&mut pos) as usize;
        let _groups = u32_at(&mut pos);
        let _dtype = u32_at(&mut pos);

        let mut blobs = Vec::new();
        for _ in 0..5 {
            let len = u32_at(&mut pos) as usize;
            blobs.push(&blob[pos..pos + len]);
            pos += len;
        }
        if name == want_name {
            let expected = blobs[3]
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            return (cols, expected);
        }
    }
    panic!("fikstura nie zawiera {want_name}");
}

#[test]
fn hf_flavour_is_still_selected_for_transformers_checkpoints() {
    // Wariant MLX nie może przejąć ścieżki HF: obie muszą dalej istnieć.
    let hf = serde_json::json!({"d_model": 384, "encoder_layers": 4});
    assert_eq!(
        WhisperFlavour::detect(&hf).unwrap(),
        WhisperFlavour::HfTransformers
    );
    let names = WhisperFlavour::HfTransformers.names();
    assert_eq!(names.q, "q_proj");
    assert_eq!(names.enc_block, "model.encoder.layers");
    assert!(names.enc_pos.is_some(), "HF przechowuje pozycje enkodera");
}
