// ===== File: real_models.rs — integration tests against real on-disk models =====
//
// These tests exercise the loaders on actual model files from the TentaFlow
// runtime. They skip cleanly (with a note) when the files are absent so CI
// machines without the models stay green.

use std::path::{Path, PathBuf};

use forge_formats::{
    dequantize_to_f32, Gguf, HfConfig, ModelDescriptor, NvFp4Scheme, NvFp4TensorNames, SafeTensors,
    WeightRole,
};
use forge_types::{DType, QuantKind};

fn repo_root() -> PathBuf {
    // crates/forge-formats -> tentaflow-infer -> TentaFlow
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives two levels below the workspace root")
        .parent()
        .expect("workspace root has a parent")
        .to_path_buf()
}

#[test]
fn parse_qwen3_gguf_and_dequant_real_tensor() {
    let path = repo_root().join(".runtime/models/model.gguf");
    if !path.is_file() {
        eprintln!("skipping: {} not present", path.display());
        return;
    }

    let gguf = Gguf::open(&path).expect("parse GGUF");
    assert_eq!(gguf.version, 3);
    assert_eq!(gguf.tensors().len(), 310);
    assert_eq!(gguf.get_str("general.architecture"), Some("qwen3"));
    assert_eq!(gguf.get_u32("qwen3.block_count"), Some(28));
    assert_eq!(gguf.get_u32("qwen3.attention.head_count"), Some(16));
    assert_eq!(gguf.get_u32("qwen3.attention.head_count_kv"), Some(8));
    assert!(gguf.get_str("tokenizer.chat_template").is_some());
    assert_eq!(
        gguf.get_array("tokenizer.ggml.tokens").map(|a| a.len()),
        Some(151936)
    );

    let desc = ModelDescriptor::detect(&gguf).expect("detect arch");
    assert_eq!(desc.arch, "qwen3");
    assert_eq!(desc.params.block_count, 28);
    assert_eq!(desc.params.n_heads, 16);
    assert_eq!(desc.params.n_kv_heads, 8);
    assert_eq!(desc.params.head_dim, 128);
    assert_eq!(desc.params.hidden_size, 1024);
    assert_eq!(desc.params.vocab_size, 151936);
    // Embedding model: no output.weight -> tied lm_head.
    assert!(desc.params.tie_word_embeddings);
    assert_eq!(desc.layers.len(), 28);
    let q_name = &desc.layers[0][&WeightRole::AttnQ];
    assert_eq!(q_name, "blk.0.attn_q.weight");

    // Dequantize a real quantized tensor and sanity-check the values.
    let t = gguf.tensor("blk.0.attn_k.weight").expect("tensor exists");
    assert_eq!(t.quant, QuantKind::Q5K);
    let data = gguf.tensor_data(&t.name).expect("tensor data view");
    assert_eq!(data.len(), t.size_bytes);
    let numel = t.numel() as usize;
    let values = dequantize_to_f32(t.dtype, t.quant, data, numel).expect("dequant q5_k");
    assert_eq!(values.len(), numel);
    assert!(values.iter().all(|v| v.is_finite()));
    let rms = (values.iter().map(|v| v * v).sum::<f32>() / numel as f32).sqrt();
    assert!(rms > 1e-4 && rms < 10.0, "implausible weight rms {rms}");

    // Also dequantize a plain f32 norm weight through the same entry point.
    let norm = gguf.tensor("output_norm.weight").expect("norm exists");
    assert_eq!(norm.dtype, DType::F32);
    let norm_vals = dequantize_to_f32(
        norm.dtype,
        norm.quant,
        gguf.tensor_data(&norm.name).unwrap(),
        norm.numel() as usize,
    )
    .expect("plain f32 view");
    assert!(norm_vals.iter().all(|v| v.is_finite()));
}

#[test]
fn parse_bielik_nvfp4_and_dequant_real_weight() {
    let snapshots =
        repo_root().join(".runtime/models/models--TentaFlow--Bielik-1.5B-NVFP4/snapshots");
    let Some(snapshot) = snapshots
        .read_dir()
        .ok()
        .and_then(|mut it| it.find_map(|e| e.ok().map(|e| e.path())))
    else {
        eprintln!("skipping: {} not present", snapshots.display());
        return;
    };

    let config = HfConfig::load(snapshot.join("config.json")).expect("parse config.json");
    assert_eq!(config.architectures, vec!["LlamaForCausalLM"]);
    assert_eq!(config.hidden_size, 1536);
    assert_eq!(config.num_hidden_layers, 32);
    assert_eq!(config.num_key_value_heads(), 2);
    assert_eq!(config.head_dim(), 128);
    assert_eq!(config.torch_dtype.as_deref(), Some("bfloat16"));

    let scheme = NvFp4Scheme::detect(&config).expect("NVFP4 scheme detected");
    assert_eq!(scheme.group_size, 16);
    assert!(scheme.ignore.iter().any(|m| m == "lm_head"));

    let desc = ModelDescriptor::from_hf(&config).expect("descriptor from HF config");
    assert_eq!(desc.arch, "llama");
    assert_eq!(desc.params.block_count, 32);
    assert_eq!(desc.params.rope_theta, 1_000_000.0);
    // lm_head is untied AND excluded from quantization -> plain weight.
    assert_eq!(desc.globals[&WeightRole::LmHead], "lm_head.weight");

    let st = SafeTensors::open(snapshot.join("model.safetensors")).expect("open safetensors");
    let weight_name = &desc.layers[0][&WeightRole::AttnQ];
    let names = NvFp4TensorNames::for_weight(weight_name).expect("nvfp4 names");
    let packed = st.tensor(&names.packed).expect("packed tensor");
    assert_eq!(packed.dtype, DType::U8);
    assert_eq!(packed.shape, vec![1536, 768]);
    let scale = st.tensor(&names.scale).expect("scale tensor");
    assert_eq!(scale.dtype, DType::F8E4M3);
    assert_eq!(scale.shape, vec![1536, 96]);
    let gs_bytes = st.data(&names.global_scale).expect("global scale data");
    assert_eq!(gs_bytes.len(), 4);
    let global_scale = f32::from_le_bytes(gs_bytes.try_into().unwrap());
    assert!(global_scale.is_finite() && global_scale > 0.0);

    let rows = packed.shape[0];
    let cols = packed.shape[1] * 2;
    let values = forge_formats::nvfp4::dequantize_nvfp4(
        st.data(&names.packed).unwrap(),
        st.data(&names.scale).unwrap(),
        global_scale,
        rows,
        cols,
        scheme.group_size,
    )
    .expect("nvfp4 dequant");
    assert_eq!(values.len(), rows * cols);
    assert!(values.iter().all(|v| v.is_finite()));
    let nonzero = values.iter().filter(|v| **v != 0.0).count();
    assert!(nonzero > values.len() / 4, "weight is mostly zeros");
    let rms = (values.iter().map(|v| v * v).sum::<f32>() / values.len() as f32).sqrt();
    assert!(rms > 1e-4 && rms < 1.0, "implausible weight rms {rms}");

    // The unquantized lm_head stays BF16 and converts through the plain path.
    let head = st.tensor("lm_head.weight").expect("lm_head present");
    assert_eq!(head.dtype, DType::BF16);
    let head_row = &st.data("lm_head.weight").unwrap()[..1536 * 2];
    let head_vals =
        dequantize_to_f32(DType::BF16, QuantKind::None, head_row, 1536).expect("bf16 row");
    assert!(head_vals.iter().all(|v| v.is_finite()));
}
