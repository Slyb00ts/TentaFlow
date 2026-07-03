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
    #[error("stage '{stage_id}': referenced stage '{referenced}' must be op=detect with input=frame")]
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
                if parent.op != CvOp::Detect
                    || !matches!(parent.input, CvStageInput::Frame { .. })
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
            {"stage_id":"ocr_plate","op":"ocr","model":"tentavision-ocr","input":{"kind":"stage","stage_id":"detect","classes":["tablica_rejestracyjna"]},"params":{"ocr_mode":"plate","crop_pad_x":0.15,"crop_pad_y":0.1},"output":"tekst"},
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
