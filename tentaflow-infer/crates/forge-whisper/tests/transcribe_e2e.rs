// ===== File: transcribe_e2e.rs — GPU end-to-end transcription of test-models/jfk.wav =====
// Requires a CUDA device plus the whisper-base snapshot in test-models/, so
// it is #[ignore]d by default: `cargo test -p forge-whisper -- --ignored`.

use std::path::PathBuf;
use std::sync::Arc;

use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_whisper::{audio, WhisperModel};

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

#[test]
#[ignore = "needs a CUDA GPU and test-models/whisper-base"]
fn transcribes_jfk() {
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: 768 << 20,
            kv_cache: 4 << 20,
            activations: 32 << 20,
            kv_page_size: 256 << 10,
        },
    )
    .expect("CUDA device");
    let dev: Arc<dyn Device> = device;

    let mut model =
        WhisperModel::load(dev, repo_path("test-models/whisper-base")).expect("load whisper-base");
    let samples = audio::load_wav(repo_path("test-models/jfk.wav")).expect("load jfk.wav");

    let text = model.transcribe(&samples, Some("en")).expect("transcribe");
    println!("transcript: {text}");
    assert!(
        text.to_lowercase().contains("ask not what your country"),
        "unexpected transcript: {text}"
    );
}
