//! The stored shape of an SLA policy and of an assignment, and the rules a
//! policy must satisfy. The same bounds are CHECK constraints of migration 211;
//! validating here turns a constraint failure into an error the administrator
//! can act on.

use super::repository::SlaError;

/// The only assignable kind for now; the column is a CHECK-ed list so more
/// kinds can be added later by a migration.
pub const SLA_TARGET_FLOW: &str = "flow";

/// Windows a call limit can be counted in, in seconds.
pub const CALL_WINDOWS_SECS: [u32; 4] = [1, 60, 3600, 86_400];

/// Longest name; the same bound is a CHECK in migration 211.
pub const MAX_NAME_CHARS: usize = 200;

/// Longest description; the same bound is a CHECK in migration 211.
pub const MAX_DESCRIPTION_CHARS: usize = 2000;

/// How a parallel bus consumer orders the records it hands to the flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BusOrdering {
    /// Units of one partition run one after another, partitions in parallel.
    #[default]
    Partition,
    /// Every unit runs in parallel, no order is kept.
    None,
}

impl BusOrdering {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Partition => "partition",
            Self::None => "none",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "partition" => Some(Self::Partition),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// What an administrator edits. `None` means "no such limit".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SlaPolicySpec {
    pub name: String,
    pub description: String,
    pub max_concurrency: Option<u32>,
    pub max_queue: Option<u32>,
    pub queue_timeout_ms: Option<u32>,
    pub call_limit: Option<u32>,
    pub call_window_secs: Option<u32>,
    pub timeout_ms: Option<u32>,
    pub latency_p95_ms: Option<u32>,
    pub error_rate_pct: Option<u32>,
    pub warn_at_pct: Option<u32>,
    pub ordering: BusOrdering,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlaPolicy {
    pub id: String,
    pub org_id: String,
    pub spec: SlaPolicySpec,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub updated_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlaAssignment {
    pub target_kind: String,
    pub target_id: String,
    pub policy_id: String,
    pub org_id: String,
    pub assigned_at_ms: i64,
    pub assigned_by: String,
}

fn invalid(field: &'static str, reason: impl Into<String>) -> SlaError {
    SlaError::Invalid {
        field,
        reason: reason.into(),
    }
}

fn in_range(field: &'static str, value: Option<u32>, min: u32, max: u32) -> Result<(), SlaError> {
    match value {
        Some(v) if v < min || v > max => Err(invalid(
            field,
            format!("must be between {min} and {max}, got {v}"),
        )),
        _ => Ok(()),
    }
}

impl SlaPolicySpec {
    pub fn validate(&self) -> Result<(), SlaError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(invalid("name", "must not be empty"));
        }
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(invalid(
                "name",
                format!("must be at most {MAX_NAME_CHARS} characters"),
            ));
        }
        if self.description.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(invalid(
                "description",
                format!("must be at most {MAX_DESCRIPTION_CHARS} characters"),
            ));
        }
        in_range("max_concurrency", self.max_concurrency, 1, 1024)?;
        in_range("max_queue", self.max_queue, 0, 10_000)?;
        in_range("queue_timeout_ms", self.queue_timeout_ms, 1, 300_000)?;
        in_range("call_limit", self.call_limit, 1, u32::MAX)?;
        in_range("timeout_ms", self.timeout_ms, 100, 86_400_000)?;
        in_range("latency_p95_ms", self.latency_p95_ms, 1, u32::MAX)?;
        in_range("error_rate_pct", self.error_rate_pct, 1, 100)?;
        in_range("warn_at_pct", self.warn_at_pct, 1, 99)?;
        match (self.call_limit, self.call_window_secs) {
            (Some(_), None) => {
                return Err(invalid(
                    "call_window_secs",
                    "required together with call_limit",
                ))
            }
            (None, Some(_)) => {
                return Err(invalid(
                    "call_limit",
                    "required together with call_window_secs",
                ))
            }
            (Some(_), Some(window)) if !CALL_WINDOWS_SECS.contains(&window) => {
                return Err(invalid(
                    "call_window_secs",
                    "must be 1, 60, 3600 or 86400 seconds",
                ))
            }
            _ => {}
        }
        Ok(())
    }
}
