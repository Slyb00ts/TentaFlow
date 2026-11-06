// ============ File: retry_class.rs — BPMN B4.4 retry class of every incident code the engine raises ============

use tentaflow_protocol::processes::ProcessRetryDenied;

/// What a retry of an incident with this code is when nothing about its job
/// proves otherwise. The proof of a job (no effect outside) is state, so it is
/// read from the database; the code only says what kind of failure it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RetryClass {
    /// A failure of a task: retryable once its invocation is proven unexecuted.
    Job,
    /// The outside effect may have happened.
    JobUncertain,
    /// The result is accepted; only its mapping into variables failed.
    OutputMapping,
    /// No retry, and the reason a reader is told.
    Never(ProcessRetryDenied),
}

/// Every code the engine raises as an incident has an explicit entry here.
/// `None` is a code this table has never heard of (a legacy or foreign row),
/// which is shown as not retryable without a reason. The completeness test
/// reads the engine sources, so a new code cannot be added without a decision.
pub(super) fn classify_code(code: &str) -> Option<RetryClass> {
    use ProcessRetryDenied as Denied;
    use RetryClass::{Job, JobUncertain, Never, OutputMapping};
    Some(match code {
        "AGENT_GOAL_INVALID" | "AGENT_HARNESS_UNAVAILABLE" | "AGENT_NO_RESULT"
        | "AGENT_RESULT_UNREADABLE" | "AGENT_SPAWN_FAILED" | "AGENT_UNAVAILABLE"
        | "BLOCK_INPUT_INVALID" | "BLOCK_UNAVAILABLE" | "FLOW_ERROR" | "FLOW_UNAVAILABLE"
        | "INTERRUPTED" | "INVALID_SERVICE_JOB" | "LEASE_LOST" | "SERVICE_ERROR"
        | "SERVICE_SOURCE_UNREADABLE" | "SERVICE_TIMEOUT" | "STALE_WORKER"
        | "TOOL_ARGUMENTS_INVALID" | "TOOL_BUSY" | "TOOL_ERROR" | "TOOL_PERMISSION_DENIED"
        | "TOOL_UNAVAILABLE" | "UNBOUND_IMPLEMENTATION" | "WORKER_ERROR" => Job,

        "EXTERNAL_OUTCOME_UNCERTAIN" | "EXTERNAL_RESULT_NOT_APPLIED" => JobUncertain,

        "ACTIVITY_IO_OUTPUT_FAILED" | "SCRIPT_MAPPING_FAILED" => OutputMapping,

        "SOURCE_ACCESS_REVOKED" | "SEND_ADMISSION_AUTHORITY_DENIED" => Never(Denied::SourceUnavailable),

        "ACTIVITY_IO_INPUT_FAILED" | "AMBIGUOUS_GATEWAY" | "EXPRESSION_ERROR"
        | "IMMEDIATE_TRANSITION_LIMIT" | "INCLUSIVE_GATEWAY_ERROR" | "MESSAGE_EXPRESSION_ERROR"
        | "MESSAGE_MAPPING_ERROR" | "MESSAGE_PREDICATE_ERROR" | "NO_MATCHING_FLOW"
        | "OUTPUT_LIMIT" | "REPETITION_AGGREGATE_LIMIT" | "REPETITION_INPUT_ERROR"
        | "REPETITION_LIMIT" | "REPETITION_MAPPING_FAILED" | "RESULT_EXPRESSION_ERROR"
        | "RESULT_REJECTED" | "SCOPE_LIMIT" | "SCRIPT_EVALUATION_FAILED"
        | "SIGNAL_MAPPING_FAILED" | "SIGNAL_PAYLOAD_FAILED" | "SIGNAL_PAYLOAD_LIMIT"
        | "SIGNAL_PENDING_LIMIT" | "SIGNAL_RECIPIENT_LIMIT" | "TOKEN_QUOTA_EXCEEDED" => {
            Never(Denied::SameFailureRepeats)
        }

        "HUMAN_REJECTED" | "VERIFICATION_AGENT_REFUSED" | "VERIFICATION_AGENT_UNAVAILABLE"
        | "VERIFICATION_FAILED" | "VERIFICATION_FLOW_UNAVAILABLE" | "VERIFICATION_UNAVAILABLE" => {
            Never(Denied::NeedsPerson)
        }

        "RESULT_ACCEPTANCE_FAILED" | "RESULT_RECOVERY_FAILED" | "REVISION_CONFLICT"
        | "STORE_BUSY" | "TOOL_RESULT_UNDECIDED" => Never(Denied::Automatic),

        "CALL_ADMISSION_ERROR" | "CALL_CHILD_CANCELLED" | "CALL_CHILD_ERROR"
        | "CALL_RETURN_ERROR" | "ESCALATION_HANDLER_FAILED" | "SCOPE_RETURN_ERROR"
        | "SCRIPT_INFRASTRUCTURE_FAILED" | "SEND_ADMISSION_TARGET_UNAVAILABLE"
        | "SIGNAL_DELIVERY_FAILED" | "TIMER_ERROR" | "TRANSITION_ERROR" => {
            Never(Denied::OtherMechanism)
        }

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use super::*;

    /// Literals of the sources that read like an incident code but are import
    /// diagnostics, simulation verdicts, expression lints or test labels.
    fn is_not_an_incident(code: &str) -> bool {
        const ELSEWHERE: [&str; 6] = [
            "LARGE_REASON", "LATE_ERROR", "OTHER_HANDLER_FAILURE", "OUT_DIR",
            "UNSUPPORTED_OR_INVALID_BPMN", "ESCALATION_IMMEDIATE_PATH_UNSUPPORTED",
        ];
        code.starts_with("CAMUNDA_") || code.starts_with("EXPR_") || code.starts_with("SIMULATION_")
            || ELSEWHERE.contains(&code)
            || (code.len() == 3 && code.as_bytes()[1] == b'_')
    }

    fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "tests") {
                    source_files(&path, out);
                }
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.ends_with(".rs") && !name.ends_with("_tests.rs") && name != "tests.rs" {
                out.push(path);
            }
        }
    }

    /// Every upper-snake-case literal of the engine sources, the shape of an
    /// incident code (the single word `INTERRUPTED` is checked by name).
    fn literals_in_sources() -> (BTreeSet<String>, String) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/processes");
        let mut files = Vec::new();
        source_files(&root, &mut files);
        assert!(files.len() > 10, "found {} source files", files.len());
        let shape = regex::Regex::new(r#""([A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+)""#).unwrap();
        let mut all = String::new();
        let mut found = BTreeSet::new();
        for file in files {
            if file.ends_with("retry_class.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            found.extend(shape.captures_iter(&text).map(|capture| capture[1].to_owned()));
            all.push_str(&text);
        }
        (found, all)
    }

    #[test]
    fn every_incident_code_the_engine_can_raise_has_a_retry_class() {
        let (literals, _) = literals_in_sources();
        let codes: Vec<&String> = literals.iter().filter(|code| !is_not_an_incident(code)).collect();
        assert!(codes.len() > 60, "found {} codes", codes.len());
        let unclassified: Vec<&&String> = codes.iter().filter(|code| classify_code(code).is_none()).collect();
        assert!(unclassified.is_empty(),
            "classify these incident codes in retry_class.rs (or list them as not incidents): {unclassified:?}");
    }

    #[test]
    fn the_table_holds_no_code_the_engine_never_raises() {
        let (_, all) = literals_in_sources();
        let source = include_str!("retry_class.rs");
        let table = source.split("#[cfg(test)]").next().unwrap();
        let quoted = regex::Regex::new(r#""([A-Z][A-Z0-9_]+)""#).unwrap();
        let listed: BTreeSet<String> = quoted.captures_iter(table)
            .map(|capture| capture[1].to_owned())
            .filter(|code| classify_code(code).is_some())
            .collect();
        assert!(listed.len() > 60, "listed {} codes", listed.len());
        let stale: Vec<&String> = listed.iter()
            .filter(|code| !all.contains(&format!("\"{code}\"")))
            .collect();
        assert!(stale.is_empty(), "codes classified but not present in the sources: {stale:?}");
    }

    #[test]
    fn the_classes_follow_the_plan() {
        for code in ["EXTERNAL_OUTCOME_UNCERTAIN", "EXTERNAL_RESULT_NOT_APPLIED"] {
            assert_eq!(classify_code(code), Some(RetryClass::JobUncertain), "{code}");
        }
        for code in ["ACTIVITY_IO_OUTPUT_FAILED", "SCRIPT_MAPPING_FAILED"] {
            assert_eq!(classify_code(code), Some(RetryClass::OutputMapping), "{code}");
        }
        for code in ["SERVICE_ERROR", "FLOW_ERROR", "INTERRUPTED", "TOOL_UNAVAILABLE", "WORKER_ERROR"] {
            assert_eq!(classify_code(code), Some(RetryClass::Job), "{code}");
        }
        for code in ["SOURCE_ACCESS_REVOKED", "ACTIVITY_IO_INPUT_FAILED", "NO_MATCHING_FLOW",
            "AMBIGUOUS_GATEWAY", "EXPRESSION_ERROR", "IMMEDIATE_TRANSITION_LIMIT", "TIMER_ERROR",
            "VERIFICATION_FAILED", "STORE_BUSY", "SCOPE_LIMIT"] {
            assert!(matches!(classify_code(code), Some(RetryClass::Never(_))), "{code}");
        }
        assert_eq!(classify_code("not-a-code-the-engine-knows"), None);
    }
}
