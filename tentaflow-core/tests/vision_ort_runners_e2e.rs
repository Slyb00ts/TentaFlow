// =============================================================================
// File: tests/vision_ort_runners_e2e.rs — ort state classifier + plate OCR e2e
// =============================================================================
//
// Proves the ort-backed camera-CV runners (Workstream-1 Chunk 2) actually load
// their ONNX models into a session pool and run one forward off the Burn/wgpu
// thread. `#[ignore]` + env-gated so a normal `cargo test` never touches ort/GPU
// or the provisioned model files.
//
// Run against a real deploy (models under `<home>/models/vision/`):
//   TENTAFLOW_VISION_ORT_TEST=/path/to/.runtime \
//     cargo test --features inference-vision-gpu,inference-supertonic \
//       --test vision_ort_runners_e2e -- --ignored --nocapture
//
// The TensorRT/CUDA EPs register softly, so on a machine without a GPU the run
// falls back to the CPU EP — the assertions (valid class, non-panicking plate
// read) hold on any backend.

#![cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]

use tentaflow_core::vision::classifier_stan::StateClassifier;
use tentaflow_core::vision::ocr_plate::PlateOcr;

/// Home dir whose `models/vision/` holds the provisioned ONNX models, or `None`
/// when the gate env var is unset (test then no-ops even under `--ignored`).
fn gate_home() -> Option<String> {
    std::env::var("TENTAFLOW_VISION_ORT_TEST")
        .ok()
        .filter(|v| !v.is_empty())
}

/// A synthetic mid-gray RGB24 crop of `w*h` — a fixed, GPU-independent input so
/// the forward exercises the whole preprocess→session.run→postprocess path.
fn gray_crop(w: u32, h: u32) -> Vec<u8> {
    vec![128u8; (w * h * 3) as usize]
}

#[test]
#[ignore = "needs ort dylib + provisioned vision models; env-gated"]
fn ort_state_classifier_loads_and_classifies() {
    let Some(home) = gate_home() else {
        eprintln!("TENTAFLOW_VISION_ORT_TEST unset — skipping");
        return;
    };
    std::env::set_var("TENTAFLOW_HOME", &home);

    let clf = StateClassifier::load().expect("load ort state classifier");
    // Pool + forward on a single crop → exactly one label from the 4-class set.
    let stan = clf
        .classify(&gray_crop(96, 96), 96, 96)
        .expect("classify crop");
    assert_eq!(
        stan.len(),
        1,
        "single-label classifier returns exactly one tag"
    );
    let allowed = ["czysta", "brudna", "uszkodzona", "nieczytelna"];
    assert!(
        allowed.contains(&stan[0].as_str()),
        "classifier returned '{}', not one of {allowed:?}",
        stan[0]
    );

    // Second call proves the pooled session is reusable (no move-out / poison).
    let again = clf
        .classify(&gray_crop(64, 32), 64, 32)
        .expect("classify again");
    assert_eq!(again.len(), 1);
}

#[test]
#[ignore = "needs ort dylib + provisioned vision models; env-gated"]
fn ort_plate_ocr_loads_and_reads() {
    let Some(home) = gate_home() else {
        eprintln!("TENTAFLOW_VISION_ORT_TEST unset — skipping");
        return;
    };
    std::env::set_var("TENTAFLOW_HOME", &home);

    let ocr = PlateOcr::load().expect("load ort plate OCR");
    // A blank crop is unlikely to be a valid PL plate; the contract is that the
    // forward + decode + PL validation run without panicking and yield Ok — the
    // synthetic input legitimately validates to `None`.
    let plate = ocr
        .read(&gray_crop(140, 70), 140, 70)
        .expect("plate read runs");
    assert!(
        plate.is_none(),
        "blank crop must not validate as a plate: {plate:?}"
    );
    // Re-run proves the pooled session is reusable across reads.
    let _ = ocr
        .read(&gray_crop(200, 60), 200, 60)
        .expect("second plate read runs");
}
