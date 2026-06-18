// ============================================================================
// File: tests/tts_embedded_lazy_load.rs — embedded sherpa-onnx TTS loads +
//       registers under the manifest `engine.id`, so a `running` embedded TTS
//       service actually synthesizes instead of "engine nie zarejestrowany".
// ============================================================================
//
// Regression guard for the embedded TTS load path dropped during the services
// refactor (commit 4f964408): the old deploy runner downloaded + registered the
// sherpa-onnx VITS engine; the new EmbeddedDeploy did not, so embedded TTS
// services were `running` but `synthesize` failed with
// "TTS engine 'sherpa-onnx' nie zarejestrowany".
//
// Requires the `inference-sherpa` feature AND network access (downloads the
// VITS Piper bundle on first run). Marked `#[ignore]`. Run manually:
//
//   cargo test --manifest-path tentaflow-core/Cargo.toml \
//     --features inference-sherpa \
//     --test tts_embedded_lazy_load -- --ignored --nocapture

#![cfg(feature = "inference-sherpa")]

use tentaflow_core::tts::{ensure_embedded_engine_loaded, shared_tts_manager, SynthesizeParams};

const ENGINE_ID: &str = "sherpa-onnx";
const MODEL_REPO: &str = "WitoldG/polish_piper_models";
// Wielogłosowe repo — voice hint musi wybrac Jarvis, nie pierwszy z dysku.
const VOICE_HINT: &str = "vits-piper-pl_PL-jarvis_wg_glos-medium";

#[tokio::test]
#[ignore = "needs inference-sherpa + network (downloads VITS bundle); run with --ignored"]
async fn embedded_sherpa_loads_and_registers_under_engine_id() {
    assert!(
        !shared_tts_manager().read().await.has(ENGINE_ID),
        "engine must not be registered before the first load"
    );

    ensure_embedded_engine_loaded(ENGINE_ID, MODEL_REPO, Some(VOICE_HINT))
        .await
        .expect("ensure_embedded_engine_loaded must download + register the sherpa engine");

    assert!(
        shared_tts_manager().read().await.has(ENGINE_ID),
        "engine must be registered under the manifest engine.id (what execute_tts looks up)"
    );
    // Voice selection from the multi-voice repo is covered by the deterministic
    // unit test `pick_onnx_for_voice_prefers_matching_voice` in tts::sherpa.

    // Second call is a no-op (idempotent) — must not error or re-register.
    ensure_embedded_engine_loaded(ENGINE_ID, MODEL_REPO, Some(VOICE_HINT))
        .await
        .expect("second ensure call must be a no-op");

    // Synthesize a short utterance — proves the registered engine actually runs
    // and the executor's `synthesize(engine_id, ..)` lookup succeeds.
    let result = shared_tts_manager()
        .read()
        .await
        .synthesize(
            ENGINE_ID,
            SynthesizeParams {
                text: "Cześć Jarvis.".to_string(),
                speaker_id: 0,
                speed: 1.0,
            },
        )
        .expect("synthesize must succeed for the registered engine");

    assert!(
        !result.samples.is_empty(),
        "synthesis must produce PCM samples"
    );
    assert!(
        result.sample_rate > 0,
        "synthesis must report a sample rate"
    );
    println!(
        "sherpa synthesized {} samples @ {} Hz",
        result.samples.len(),
        result.sample_rate
    );
}
