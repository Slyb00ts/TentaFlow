// =============================================================================
// File: services/camera_ingest/cv_pipeline.rs — configurable camera CV
// pipeline (JSON stored in `camera_cv_pipelines`): serde types, structural
// validator and detection-class matching. Pure data module (no GPU/GStreamer),
// compiles without the `inference-vision-gpu` feature.
//
// Example:
//   let p: CvPipeline = serde_json::from_str(json)?;
//   cv_pipeline::validate(&p)?;
//   cv_pipeline::validate_aliases(&conn, &p)?; // at save time (repository)
// =============================================================================

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

/// Hard cap on the stage count: the pipeline is depth-2 (frame -> detect ->
/// per-crop stage), so anything beyond a handful of stages is a config error,
/// not a real workload.
const MAX_STAGES: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvPipeline {
    pub stages: Vec<CvStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvStage {
    pub stage_id: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub op: CvOp,
    /// Model alias (`model_aliases.alias`) — never a raw preset id, so swapping
    /// the model is an alias edit, not a pipeline edit.
    pub model: String,
    pub input: CvStageInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub params: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<CvStageOutput>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CvOp {
    Detect,
    Classify,
    Ocr,
    Embed,
}

impl fmt::Display for CvOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            CvOp::Detect => "detect",
            CvOp::Classify => "classify",
            CvOp::Ocr => "ocr",
            CvOp::Embed => "embed",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CvStageInput {
    /// Full camera frame. `fps` omitted = the engine keeps pacing by
    /// `cameras.analysis_fps` (the pre-pipeline behavior).
    Frame {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fps: Option<u32>,
    },
    /// Crops of the referenced detect stage's detections whose class matches
    /// one of `classes` (see [`class_matches`]).
    Stage {
        stage_id: String,
        #[serde(default)]
        classes: Vec<String>,
    },
}

/// Which `Detection` field the stage result lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CvStageOutput {
    Stan,
    Tekst,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CvPipelineError {
    #[error("pipeline must have between 1 and {MAX_STAGES} stages, got {0}")]
    StageCount(usize),
    #[error("stage_id '{0}' is invalid: expected 1-64 chars of [a-z0-9_-]")]
    InvalidStageId(String),
    #[error("duplicate stage_id '{0}'")]
    DuplicateStageId(String),
    #[error("stage '{0}': model alias must not be empty")]
    EmptyModel(String),
    #[error("stage '{stage_id}': op '{op}' requires input kind '{expected}'")]
    InputKindMismatch {
        stage_id: String,
        op: CvOp,
        expected: &'static str,
    },
    #[error("stage '{0}' references itself")]
    SelfReference(String),
    #[error("stage '{stage_id}' references unknown stage '{referenced}'")]
    UnknownStageRef {
        stage_id: String,
        referenced: String,
    },
    #[error(
        "stage '{stage_id}': referenced stage '{referenced}' must be op=detect with input=frame"
    )]
    InvalidStageRef {
        stage_id: String,
        referenced: String,
    },
    #[error("stage '{0}': threshold is only valid on op=detect")]
    ThresholdNotAllowed(String),
    #[error("stage '{0}': threshold {1} out of range 0.0..=1.0")]
    ThresholdOutOfRange(String, f32),
    #[error("stage '{0}': fps {1} out of range 0..=60")]
    FpsOutOfRange(String, u32),
    #[error("stage '{stage_id}': invalid class pattern '{pattern}' ('*' only as trailing char, non-empty)")]
    InvalidClassPattern { stage_id: String, pattern: String },
    #[error("stage '{stage_id}': op '{op}' requires output '{expected}'")]
    OutputMismatch {
        stage_id: String,
        op: CvOp,
        expected: &'static str,
    },
    #[error("stage '{stage_id}': op '{op}' must not declare an output")]
    OutputNotAllowed { stage_id: String, op: CvOp },
    #[error("stage '{stage_id}': param '{param}' value {value} out of range 0.0..=0.5")]
    CropPadOutOfRange {
        stage_id: String,
        param: &'static str,
        value: f64,
    },
    #[error("stage '{stage_id}': param '{param}' must be a number")]
    ParamNotNumeric {
        stage_id: String,
        param: &'static str,
    },
    #[error("stage '{0}': ocr_mode must be one of plate|adr|generic")]
    InvalidOcrMode(String),
    #[error("unknown model aliases: {0}")]
    UnknownAliases(String),
}

fn is_valid_stage_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Matches a detection class against stage class patterns: exact match or a
/// trailing-`*` prefix match (e.g. `nalepka*` matches `nalepka_adr_3`).
pub fn class_matches(patterns: &[String], class: &str) -> bool {
    patterns.iter().any(|p| match p.strip_suffix('*') {
        Some(prefix) => class.starts_with(prefix),
        None => p == class,
    })
}

/// Structural validation of a pipeline — DB-free by design so it is unit
/// testable; alias existence is checked separately by [`validate_aliases`]
/// at save time.
pub fn validate(p: &CvPipeline) -> Result<(), CvPipelineError> {
    if p.stages.is_empty() || p.stages.len() > MAX_STAGES {
        return Err(CvPipelineError::StageCount(p.stages.len()));
    }

    let mut seen: HashSet<&str> = HashSet::new();
    for stage in &p.stages {
        if !is_valid_stage_id(&stage.stage_id) {
            return Err(CvPipelineError::InvalidStageId(stage.stage_id.clone()));
        }
        if !seen.insert(stage.stage_id.as_str()) {
            return Err(CvPipelineError::DuplicateStageId(stage.stage_id.clone()));
        }
    }

    for stage in &p.stages {
        let sid = stage.stage_id.clone();
        if stage.model.trim().is_empty() {
            return Err(CvPipelineError::EmptyModel(sid));
        }

        match (&stage.op, &stage.input) {
            (CvOp::Detect, CvStageInput::Frame { fps }) => {
                if let Some(fps) = fps {
                    if *fps > 60 {
                        return Err(CvPipelineError::FpsOutOfRange(sid, *fps));
                    }
                }
            }
            (CvOp::Detect, CvStageInput::Stage { .. }) => {
                return Err(CvPipelineError::InputKindMismatch {
                    stage_id: sid,
                    op: stage.op,
                    expected: "frame",
                });
            }
            (_, CvStageInput::Frame { .. }) => {
                return Err(CvPipelineError::InputKindMismatch {
                    stage_id: sid,
                    op: stage.op,
                    expected: "stage",
                });
            }
            (_, CvStageInput::Stage { stage_id, classes }) => {
                if stage_id == &stage.stage_id {
                    return Err(CvPipelineError::SelfReference(sid));
                }
                let Some(parent) = p.stages.iter().find(|s| &s.stage_id == stage_id) else {
                    return Err(CvPipelineError::UnknownStageRef {
                        stage_id: sid,
                        referenced: stage_id.clone(),
                    });
                };
                // Depth-2 invariant: a crop stage may only hang off a detect
                // stage that itself reads the frame — no stage chains, so
                // cycles are impossible by construction.
                if parent.op != CvOp::Detect || !matches!(parent.input, CvStageInput::Frame { .. })
                {
                    return Err(CvPipelineError::InvalidStageRef {
                        stage_id: sid,
                        referenced: stage_id.clone(),
                    });
                }
                for pattern in classes {
                    // Bare "*" would silently route EVERY detection into the
                    // stage, so a wildcard must carry a non-empty prefix.
                    let valid = match pattern.strip_suffix('*') {
                        Some(prefix) => !prefix.is_empty() && !prefix.contains('*'),
                        None => !pattern.is_empty() && !pattern.contains('*'),
                    };
                    if !valid {
                        return Err(CvPipelineError::InvalidClassPattern {
                            stage_id: sid,
                            pattern: pattern.clone(),
                        });
                    }
                }
            }
        }

        match stage.threshold {
            Some(_) if stage.op != CvOp::Detect => {
                return Err(CvPipelineError::ThresholdNotAllowed(sid));
            }
            Some(t) if !(0.0..=1.0).contains(&t) => {
                return Err(CvPipelineError::ThresholdOutOfRange(sid, t));
            }
            _ => {}
        }

        match (stage.op, stage.output) {
            (CvOp::Classify, Some(CvStageOutput::Stan)) => {}
            (CvOp::Classify, _) => {
                return Err(CvPipelineError::OutputMismatch {
                    stage_id: sid,
                    op: stage.op,
                    expected: "stan",
                });
            }
            (CvOp::Ocr, Some(CvStageOutput::Tekst)) => {}
            (CvOp::Ocr, _) => {
                return Err(CvPipelineError::OutputMismatch {
                    stage_id: sid,
                    op: stage.op,
                    expected: "tekst",
                });
            }
            (CvOp::Detect | CvOp::Embed, Some(_)) => {
                return Err(CvPipelineError::OutputNotAllowed {
                    stage_id: sid,
                    op: stage.op,
                });
            }
            (CvOp::Detect | CvOp::Embed, None) => {}
        }

        for param in ["crop_pad_x", "crop_pad_y"] {
            if let Some(value) = stage.params.get(param) {
                let Some(v) = value.as_f64() else {
                    return Err(CvPipelineError::ParamNotNumeric {
                        stage_id: sid,
                        param,
                    });
                };
                if !(0.0..=0.5).contains(&v) {
                    return Err(CvPipelineError::CropPadOutOfRange {
                        stage_id: sid,
                        param,
                        value: v,
                    });
                }
            }
        }
        if let Some(mode) = stage.params.get("ocr_mode") {
            match mode.as_str() {
                Some("plate") | Some("adr") | Some("generic") => {}
                _ => return Err(CvPipelineError::InvalidOcrMode(sid)),
            }
        }
    }

    Ok(())
}

/// Enabled hot stages: `input.kind = frame` (the validator guarantees these
/// are `op = detect`). The engine schedules one cadence per frame stage.
pub fn frame_stages(p: &CvPipeline) -> impl Iterator<Item = &CvStage> {
    p.stages
        .iter()
        .filter(|s| s.enabled && matches!(s.input, CvStageInput::Frame { .. }))
}

/// Enabled cold stages: `input.kind = stage` (per-crop classify/ocr/embed).
pub fn cold_stages(p: &CvPipeline) -> impl Iterator<Item = &CvStage> {
    p.stages
        .iter()
        .filter(|s| s.enabled && matches!(s.input, CvStageInput::Stage { .. }))
}

/// Effective analysis FPS of a frame stage: the stage's own `fps` when set,
/// otherwise the camera-level `analysis_fps` (the pre-pipeline pacing).
pub fn stage_fps(stage: &CvStage, camera_fps: u32) -> u32 {
    match stage.input {
        CvStageInput::Frame { fps: Some(fps) } => fps,
        _ => camera_fps,
    }
}

/// Position of a stage in the pipeline — merge order of per-stage results
/// (stable regardless of batch completion order). Unknown id sorts last.
pub fn stage_index(p: &CvPipeline, stage_id: &str) -> usize {
    p.stages
        .iter()
        .position(|s| s.stage_id == stage_id)
        .unwrap_or(usize::MAX)
}

/// Crop padding of a cold stage as fractions of the detection box. OCR pads
/// 30%/20%: side-view cameras see plates at an angle, so an axis-aligned detector
/// box is tight and clips characters off the near/far edge. The generous pad
/// captures the WHOLE plate for the OCR-side perspective deskew
/// (`ocr_prep::deskew_plate_rgb`), which crops back to the plate quad — so the
/// extra margin costs nothing on the common (quad-found) path and only adds
/// background on the rare no-quad fallback (the model tolerates background far
/// better than clipped glyphs). Classify/embed stay on the tight box.
pub fn crop_pads(stage: &CvStage) -> (f32, f32) {
    let default = match stage.op {
        CvOp::Ocr => (0.30, 0.20),
        _ => (0.0, 0.0),
    };
    let read = |name: &str, fallback: f32| {
        stage
            .params
            .get(name)
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(fallback)
    };
    (read("crop_pad_x", default.0), read("crop_pad_y", default.1))
}

/// OCR mode of an `op = ocr` stage (`params.ocr_mode`, validated to
/// plate|adr|generic at save time). Absent = generic text reading.
pub fn ocr_mode(stage: &CvStage) -> &str {
    stage
        .params
        .get("ocr_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("generic")
}

/// Continuous-batching flush decision. `keys` are the batch-group keys of the
/// pending items in arrival (FIFO) order — a batch NEVER mixes keys (one model
/// alias + threshold per forward). Returns the pending indices to flush:
/// - the first key (by arrival of its `max_batch`-th item) that filled a whole
///   batch, or
/// - when the oldest item exceeded the wait window (`window_elapsed`), the
///   oldest item's key group (up to `max_batch`).
/// With a single key this reduces exactly to the pre-pipeline behavior
/// (flush on full batch or on window, oldest first).
pub fn select_flush_batch<K: PartialEq>(
    keys: &[K],
    max_batch: usize,
    window_elapsed: bool,
) -> Option<Vec<usize>> {
    if keys.is_empty() || max_batch == 0 {
        return None;
    }
    let mut groups: Vec<(&K, Vec<usize>)> = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, idxs)) => {
                idxs.push(i);
                if idxs.len() >= max_batch {
                    return Some(std::mem::take(idxs));
                }
            }
            None => groups.push((key, vec![i])),
        }
    }
    if window_elapsed {
        // Groups preserve arrival order, so groups[0] is the oldest item's key.
        return groups.into_iter().next().map(|(_, idxs)| idxs);
    }
    None
}

/// Save-time check that every referenced model alias exists in
/// `model_aliases`. Kept out of [`validate`] so the structural validator stays
/// DB-free.
pub fn validate_aliases(
    conn: &rusqlite::Connection,
    p: &CvPipeline,
) -> Result<(), CvPipelineError> {
    let aliases: HashSet<&str> = p.stages.iter().map(|s| s.model.as_str()).collect();
    let mut missing: Vec<&str> = Vec::new();
    for alias in aliases {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM model_aliases WHERE alias = ?1",
                rusqlite::params![alias],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(CvPipelineError::UnknownAliases(other.to_string())),
            })?;
        if exists.is_none() {
            missing.push(alias);
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        missing.sort_unstable();
        Err(CvPipelineError::UnknownAliases(missing.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_pipeline_json() -> &'static str {
        r#"{"stages":[
            {"stage_id":"detect","op":"detect","model":"tentavision-detect","input":{"kind":"frame"},"threshold":0.5},
            {"stage_id":"stan","op":"classify","model":"tentavision-stan","input":{"kind":"stage","stage_id":"detect","classes":["nalepka*","znak_srodowiskowy","termometr","tablica_adr","tablica_rejestracyjna"]},"output":"stan"},
            {"stage_id":"ocr_plate","op":"ocr","model":"tentavision-ocr","input":{"kind":"stage","stage_id":"detect","classes":["tablica_rejestracyjna"]},"params":{"ocr_mode":"plate","crop_pad_x":0.3,"crop_pad_y":0.2},"output":"tekst"},
            {"stage_id":"ocr_adr","op":"ocr","model":"tentavision-ocr","input":{"kind":"stage","stage_id":"detect","classes":["tablica_adr"]},"params":{"ocr_mode":"adr","crop_pad_x":0.15,"crop_pad_y":0.1},"output":"tekst"}
        ]}"#
    }

    fn parse(json: &str) -> CvPipeline {
        serde_json::from_str(json).expect("pipeline json parses")
    }

    #[test]
    fn default_pipeline_parses_and_validates() {
        let p = parse(default_pipeline_json());
        assert_eq!(p.stages.len(), 4);
        assert!(p.stages.iter().all(|s| s.enabled));
        assert_eq!(p.stages[0].threshold, Some(0.5));
        validate(&p).expect("default pipeline valid");
    }

    #[test]
    fn duplicate_stage_id_rejected() {
        let mut p = parse(default_pipeline_json());
        p.stages[1].stage_id = "detect".into();
        assert_eq!(
            validate(&p),
            Err(CvPipelineError::DuplicateStageId("detect".into()))
        );
    }

    #[test]
    fn classify_with_frame_input_rejected() {
        let mut p = parse(default_pipeline_json());
        p.stages[1].input = CvStageInput::Frame { fps: None };
        assert!(matches!(
            validate(&p),
            Err(CvPipelineError::InputKindMismatch { .. })
        ));
    }

    #[test]
    fn stage_referencing_classify_parent_rejected() {
        let mut p = parse(default_pipeline_json());
        p.stages[2].input = CvStageInput::Stage {
            stage_id: "stan".into(),
            classes: vec!["tablica_rejestracyjna".into()],
        };
        assert!(matches!(
            validate(&p),
            Err(CvPipelineError::InvalidStageRef { .. })
        ));
    }

    #[test]
    fn threshold_out_of_range_rejected() {
        let mut p = parse(default_pipeline_json());
        p.stages[0].threshold = Some(1.5);
        assert_eq!(
            validate(&p),
            Err(CvPipelineError::ThresholdOutOfRange("detect".into(), 1.5))
        );
    }

    #[test]
    fn threshold_on_non_detect_rejected() {
        let mut p = parse(default_pipeline_json());
        p.stages[1].threshold = Some(0.5);
        assert_eq!(
            validate(&p),
            Err(CvPipelineError::ThresholdNotAllowed("stan".into()))
        );
    }

    #[test]
    fn inner_wildcard_pattern_rejected() {
        let mut p = parse(default_pipeline_json());
        p.stages[1].input = CvStageInput::Stage {
            stage_id: "detect".into(),
            classes: vec!["nale*pka".into()],
        };
        assert!(matches!(
            validate(&p),
            Err(CvPipelineError::InvalidClassPattern { .. })
        ));
    }

    #[test]
    fn bare_star_pattern_rejected() {
        let mut p = parse(default_pipeline_json());
        p.stages[1].input = CvStageInput::Stage {
            stage_id: "detect".into(),
            classes: vec!["*".into()],
        };
        assert!(matches!(
            validate(&p),
            Err(CvPipelineError::InvalidClassPattern { .. })
        ));
    }

    #[test]
    fn frame_and_cold_stage_selection_honors_enabled() {
        let mut p = parse(default_pipeline_json());
        assert_eq!(
            frame_stages(&p)
                .map(|s| s.stage_id.as_str())
                .collect::<Vec<_>>(),
            vec!["detect"]
        );
        assert_eq!(
            cold_stages(&p)
                .map(|s| s.stage_id.as_str())
                .collect::<Vec<_>>(),
            vec!["stan", "ocr_plate", "ocr_adr"]
        );
        p.stages[0].enabled = false;
        p.stages[2].enabled = false;
        assert_eq!(frame_stages(&p).count(), 0);
        assert_eq!(
            cold_stages(&p)
                .map(|s| s.stage_id.as_str())
                .collect::<Vec<_>>(),
            vec!["stan", "ocr_adr"]
        );
    }

    #[test]
    fn stage_fps_prefers_stage_value_over_camera_fallback() {
        let mut p = parse(default_pipeline_json());
        assert_eq!(stage_fps(&p.stages[0], 10), 10);
        p.stages[0].input = CvStageInput::Frame { fps: Some(2) };
        assert_eq!(stage_fps(&p.stages[0], 10), 2);
    }

    #[test]
    fn crop_pads_default_per_op_and_params_override() {
        let p = parse(default_pipeline_json());
        // Classify without params: tight box, like the pre-pipeline code.
        assert_eq!(crop_pads(&p.stages[1]), (0.0, 0.0));
        // OCR stages carry explicit 0.30/0.20 in the seed.
        assert_eq!(crop_pads(&p.stages[2]), (0.30, 0.20));
        // OCR without params falls back to the generous deskew-friendly default.
        let mut ocr = p.stages[2].clone();
        ocr.params = serde_json::Map::new();
        assert_eq!(crop_pads(&ocr), (0.30, 0.20));
        assert_eq!(ocr_mode(&ocr), "generic");
        assert_eq!(ocr_mode(&p.stages[2]), "plate");
        assert_eq!(ocr_mode(&p.stages[3]), "adr");
    }

    #[test]
    fn select_flush_batch_full_group_wins_and_never_mixes_keys() {
        // Single key: full batch flushes the oldest max_batch items — the
        // pre-pipeline drain(..take) behavior.
        let keys = vec!["a"; 10];
        assert_eq!(
            select_flush_batch(&keys, 8, false),
            Some((0..8).collect::<Vec<_>>())
        );
        // Mixed keys: only the group that filled a batch is selected, and
        // the batch holds one key exclusively.
        let keys = vec!["a", "b", "a", "b", "a", "b"];
        assert_eq!(select_flush_batch(&keys, 3, false), Some(vec![0, 2, 4]));
        // No full group and window not elapsed: no flush.
        assert_eq!(select_flush_batch(&keys, 4, false), None);
    }

    #[test]
    fn select_flush_batch_window_flushes_oldest_key_group() {
        let keys = vec!["b", "a", "b"];
        // Oldest item's key is "b" — its whole group flushes, "a" stays.
        assert_eq!(select_flush_batch(&keys, 8, true), Some(vec![0, 2]));
        assert_eq!(select_flush_batch::<&str>(&[], 8, true), None);
    }

    #[test]
    fn stage_index_orders_merge_by_pipeline_position() {
        let p = parse(default_pipeline_json());
        assert_eq!(stage_index(&p, "detect"), 0);
        assert_eq!(stage_index(&p, "ocr_adr"), 3);
        assert_eq!(stage_index(&p, "missing"), usize::MAX);
    }

    #[test]
    fn class_matches_exact_wildcard_and_miss() {
        let patterns = vec![
            "nalepka*".to_string(),
            "termometr".to_string(),
            "znak_srodowiskowy".to_string(),
        ];
        assert!(class_matches(&patterns, "termometr"));
        assert!(class_matches(&patterns, "nalepka_adr_3"));
        assert!(class_matches(&patterns, "nalepka"));
        assert!(!class_matches(&patterns, "tablica_adr"));
        assert!(!class_matches(&patterns, "termometry"));
        assert!(!class_matches(&[], "termometr"));
    }
}
