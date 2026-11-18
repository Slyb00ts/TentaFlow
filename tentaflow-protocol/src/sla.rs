// =============================================================================
// File: sla.rs
// Purpose: Binary CBOR protocol for service SLA policies (TB-03): the named
//          limit sets an administrator edits, their assignment to flows, the
//          live state of a governed flow and the breach history. The limits
//          themselves (concurrency, call window, timeout, thresholds) are
//          enforced on every node from replicated data; the state shown here is
//          the answering node's own.
//
//          Append-only, and a rename is the one change that breaks every
//          deployed peer while the round-trip tests stay green — ciborium tags
//          by NAME. A field added later MUST carry `#[serde(default)]`.
// Example: MessageBody::SlaBody(SlaPayload::PolicyListRequest { org_id: None })
// =============================================================================

use serde::{Deserialize, Serialize};

/// What an administrator edits. `None` means "no such limit".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SlaPolicySpecWire {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Parallel executions per governed service (1..=1024).
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    /// Callers that may wait above the concurrency limit (0..=10000).
    #[serde(default)]
    pub max_queue: Option<u32>,
    #[serde(default)]
    pub queue_timeout_ms: Option<u32>,
    /// Set together with `call_window_secs` or not at all.
    #[serde(default)]
    pub call_limit: Option<u32>,
    /// 1, 60, 3600 or 86400: a clock-aligned fixed window, counted per node.
    #[serde(default)]
    pub call_window_secs: Option<u32>,
    #[serde(default)]
    pub timeout_ms: Option<u32>,
    #[serde(default)]
    pub latency_p95_ms: Option<u32>,
    #[serde(default)]
    pub error_rate_pct: Option<u32>,
    #[serde(default)]
    pub warn_at_pct: Option<u32>,
    /// "partition" (default) or "none"; only a bus consumer reads it.
    #[serde(default = "default_ordering")]
    pub ordering: String,
}

fn default_ordering() -> String {
    "partition".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlaPolicyWire {
    pub id: String,
    #[serde(default)]
    pub org_id: String,
    pub spec: SlaPolicySpecWire,
    #[serde(default)]
    pub created_at_ms: i64,
    #[serde(default)]
    pub updated_at_ms: i64,
    #[serde(default)]
    pub updated_by: String,
    /// How many flows the policy governs. Filled by the list; `0` elsewhere.
    #[serde(default)]
    pub assigned_flows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlaAssignmentWire {
    pub flow_id: String,
    pub policy_id: String,
    #[serde(default)]
    pub assigned_at_ms: i64,
    #[serde(default)]
    pub assigned_by: String,
}

/// A refusal the screen turns into a message of its own language. `code` is
/// stable: `invalid_value` (`field` names the offending input), `name_taken`,
/// `policy_in_use` (`assignments` says how many), `system_flow`,
/// `governed_elsewhere` (another organization already governs the flow),
/// `storage_error`. `detail` is English text for logs, never for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlaErrorWire {
    pub code: String,
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub assignments: Option<u32>,
    #[serde(default)]
    pub detail: String,
}

/// The call window of one governed flow on the answering node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlaWindowWire {
    /// Calls counted against the limit in this window.
    #[serde(default)]
    pub used: u32,
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub window_secs: u32,
    /// Unix milliseconds when the window resets.
    #[serde(default)]
    pub resets_at_ms: u64,
}

/// What the evaluator saw over its last window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlaEvaluationWire {
    #[serde(default)]
    pub at_ms: u64,
    #[serde(default)]
    pub span_ms: u64,
    #[serde(default)]
    pub finished: u64,
    #[serde(default)]
    pub errors: u64,
    #[serde(default)]
    pub timeouts: u64,
    #[serde(default)]
    pub rejected_calls: u64,
    #[serde(default)]
    pub rejected_concurrency: u64,
    #[serde(default)]
    pub p95_ms: Option<f64>,
    #[serde(default)]
    pub error_rate_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlaBreachWire {
    pub id: String,
    #[serde(default)]
    pub flow_id: String,
    #[serde(default)]
    pub policy_id: String,
    /// "calls_warn" | "calls_limit" | "concurrency_saturated" | "latency" |
    /// "errors" | "timeouts".
    #[serde(default)]
    pub kind: String,
    /// "ms" | "percent" | "calls": what `threshold` and `peak_value` count.
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub threshold: f64,
    #[serde(default)]
    pub peak_value: f64,
    #[serde(default)]
    pub raised_at_ms: i64,
    /// `None` while the breach is open.
    #[serde(default)]
    pub resolved_at_ms: Option<i64>,
}

/// The state of one governed flow on the answering node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlaFlowStatusWire {
    pub flow_id: String,
    #[serde(default)]
    pub policy_id: String,
    /// "ok" | "warn" | "breach", see [`SlaBrief::state`].
    #[serde(default)]
    pub state: String,
    /// Calls running now.
    #[serde(default)]
    pub in_flight: u32,
    /// Callers waiting for a free slot.
    #[serde(default)]
    pub waiting: u32,
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    #[serde(default)]
    pub window: Option<SlaWindowWire>,
    /// `None` until the evaluator has looked at the flow.
    #[serde(default)]
    pub evaluation: Option<SlaEvaluationWire>,
    /// Refusals since the node started.
    #[serde(default)]
    pub rejected_calls_total: u64,
    #[serde(default)]
    pub rejected_concurrency_total: u64,
    /// Refusals since local midnight on the answering node.
    #[serde(default)]
    pub rejected_calls_today: u64,
    #[serde(default)]
    pub rejected_concurrency_today: u64,
    #[serde(default)]
    pub open_breaches: Vec<SlaBreachWire>,
}

/// The small form of a flow's SLA state the flow list carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlaBrief {
    /// "ok" | "warn" (only a call-window warning is open) | "breach" (any
    /// other breach is open).
    pub state: String,
    #[serde(default)]
    pub in_flight: u32,
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    #[serde(default)]
    pub window_used: Option<u32>,
    #[serde(default)]
    pub window_limit: Option<u32>,
    /// Another organization's policy governs the flow. The numbers above are
    /// what the caller's own callers meet; the policy itself (id, name) is not
    /// shown, and `sla_policy_id` of the flow stays empty.
    #[serde(default)]
    pub governed_elsewhere: bool,
}

/// Request and response of the SLA family. Reads honour `org_id` only for the
/// platform super-admin; for everyone else the caller's own organization is
/// the only one there is, whatever the request names. Writes always act on the
/// caller's organization and need `sla.manage`; assigning also needs the right
/// to edit flows. A policy or a flow of another organization answers
/// `not_found`, as one that does not exist does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SlaPayload {
    PolicyListRequest {
        #[serde(default)]
        org_id: Option<String>,
    },
    PolicyListResponse {
        #[serde(default)]
        policies: Vec<SlaPolicyWire>,
        /// Whether the caller may create, change, delete and assign.
        #[serde(default)]
        can_manage: bool,
    },
    PolicyGetRequest {
        #[serde(default)]
        policy_id: String,
        #[serde(default)]
        org_id: Option<String>,
    },
    PolicyGetResponse {
        policy: SlaPolicyWire,
        /// The flows the policy governs.
        #[serde(default)]
        flow_ids: Vec<String>,
    },
    /// `policy_id: None` creates, `Some` replaces the whole spec.
    PolicyUpsertRequest {
        #[serde(default)]
        policy_id: Option<String>,
        #[serde(default)]
        spec: SlaPolicySpecWire,
    },
    PolicyUpsertResponse {
        /// `None` when `error` says why the write was refused.
        #[serde(default)]
        policy: Option<SlaPolicyWire>,
        #[serde(default)]
        created: bool,
        #[serde(default)]
        error: Option<SlaErrorWire>,
    },
    PolicyDeleteRequest {
        #[serde(default)]
        policy_id: String,
    },
    PolicyDeleteResponse {
        #[serde(default)]
        error: Option<SlaErrorWire>,
    },
    AssignRequest {
        #[serde(default)]
        flow_id: String,
        #[serde(default)]
        policy_id: String,
    },
    AssignResponse {
        #[serde(default)]
        assignment: Option<SlaAssignmentWire>,
        #[serde(default)]
        error: Option<SlaErrorWire>,
    },
    UnassignRequest {
        #[serde(default)]
        flow_id: String,
    },
    UnassignResponse {
        /// `false` when the flow had no policy.
        #[serde(default)]
        removed: bool,
        /// `governed_elsewhere` when another organization governs the flow.
        #[serde(default)]
        error: Option<SlaErrorWire>,
    },
    /// One governed flow, or every governed flow of the organization.
    StatusRequest {
        #[serde(default)]
        flow_id: Option<String>,
        #[serde(default)]
        org_id: Option<String>,
    },
    StatusResponse {
        #[serde(default)]
        statuses: Vec<SlaFlowStatusWire>,
    },
    BreachListRequest {
        #[serde(default)]
        flow_id: Option<String>,
        #[serde(default)]
        open_only: bool,
        #[serde(default)]
        offset: u32,
        /// `0` asks for the default page; the server caps it at 200.
        #[serde(default)]
        limit: u32,
        #[serde(default)]
        org_id: Option<String>,
    },
    BreachListResponse {
        /// Newest first.
        #[serde(default)]
        breaches: Vec<SlaBreachWire>,
        /// What the filter matches, whatever the page.
        #[serde(default)]
        total: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbor;
    use crate::message_body::MessageBody;

    fn spec() -> SlaPolicySpecWire {
        SlaPolicySpecWire {
            name: "Gold".into(),
            description: "d".into(),
            max_concurrency: Some(4),
            max_queue: Some(8),
            queue_timeout_ms: Some(10_000),
            call_limit: Some(60),
            call_window_secs: Some(60),
            timeout_ms: Some(30_000),
            latency_p95_ms: Some(500),
            error_rate_pct: Some(5),
            warn_at_pct: Some(80),
            ordering: "none".into(),
        }
    }

    fn policy() -> SlaPolicyWire {
        SlaPolicyWire {
            id: "p-1".into(),
            org_id: "org-a".into(),
            spec: spec(),
            created_at_ms: 1,
            updated_at_ms: 2,
            updated_by: "u".into(),
            assigned_flows: 3,
        }
    }

    fn breach() -> SlaBreachWire {
        SlaBreachWire {
            id: "b-1".into(),
            flow_id: "f-1".into(),
            policy_id: "p-1".into(),
            kind: "latency".into(),
            unit: "ms".into(),
            node_id: "n-1".into(),
            threshold: 500.0,
            peak_value: 912.5,
            raised_at_ms: 10,
            resolved_at_ms: None,
        }
    }

    fn error() -> SlaErrorWire {
        SlaErrorWire {
            code: "policy_in_use".into(),
            field: None,
            assignments: Some(2),
            detail: "policy p-1 is assigned to 2 service(s)".into(),
        }
    }

    fn all_variants() -> Vec<SlaPayload> {
        vec![
            SlaPayload::PolicyListRequest {
                org_id: Some("org-b".into()),
            },
            SlaPayload::PolicyListResponse {
                policies: vec![policy()],
                can_manage: true,
            },
            SlaPayload::PolicyGetRequest {
                policy_id: "p-1".into(),
                org_id: None,
            },
            SlaPayload::PolicyGetResponse {
                policy: policy(),
                flow_ids: vec!["f-1".into(), "f-2".into()],
            },
            SlaPayload::PolicyUpsertRequest {
                policy_id: Some("p-1".into()),
                spec: spec(),
            },
            SlaPayload::PolicyUpsertResponse {
                policy: Some(policy()),
                created: true,
                error: None,
            },
            SlaPayload::PolicyUpsertResponse {
                policy: None,
                created: false,
                error: Some(SlaErrorWire {
                    code: "invalid_value".into(),
                    field: Some("max_concurrency".into()),
                    assignments: None,
                    detail: "must be between 1 and 1024, got 0".into(),
                }),
            },
            SlaPayload::PolicyDeleteRequest {
                policy_id: "p-1".into(),
            },
            SlaPayload::PolicyDeleteResponse {
                error: Some(error()),
            },
            SlaPayload::AssignRequest {
                flow_id: "f-1".into(),
                policy_id: "p-1".into(),
            },
            SlaPayload::AssignResponse {
                assignment: Some(SlaAssignmentWire {
                    flow_id: "f-1".into(),
                    policy_id: "p-1".into(),
                    assigned_at_ms: 5,
                    assigned_by: "u".into(),
                }),
                error: None,
            },
            SlaPayload::UnassignRequest {
                flow_id: "f-1".into(),
            },
            SlaPayload::UnassignResponse {
                removed: true,
                error: None,
            },
            SlaPayload::StatusRequest {
                flow_id: Some("f-1".into()),
                org_id: None,
            },
            SlaPayload::StatusResponse {
                statuses: vec![SlaFlowStatusWire {
                    flow_id: "f-1".into(),
                    policy_id: "p-1".into(),
                    state: "warn".into(),
                    in_flight: 2,
                    waiting: 1,
                    max_concurrency: Some(4),
                    window: Some(SlaWindowWire {
                        used: 57,
                        limit: 60,
                        window_secs: 60,
                        resets_at_ms: 1_700_000_060_000,
                    }),
                    evaluation: Some(SlaEvaluationWire {
                        at_ms: 9,
                        span_ms: 300_000,
                        finished: 100,
                        errors: 3,
                        timeouts: 1,
                        rejected_calls: 4,
                        rejected_concurrency: 5,
                        p95_ms: Some(123.5),
                        error_rate_pct: Some(3.0),
                    }),
                    rejected_calls_total: 7,
                    rejected_concurrency_total: 8,
                    rejected_calls_today: 3,
                    rejected_concurrency_today: 2,
                    open_breaches: vec![breach()],
                }],
            },
            SlaPayload::BreachListRequest {
                flow_id: None,
                open_only: true,
                offset: 50,
                limit: 25,
                org_id: None,
            },
            SlaPayload::BreachListResponse {
                breaches: vec![breach()],
                total: 51,
            },
        ]
    }

    #[test]
    fn every_sla_variant_survives_a_cbor_round_trip() {
        for payload in all_variants() {
            let body = MessageBody::SlaBody(payload);
            let bytes = cbor::encode(&body).expect("encode");
            let back: MessageBody = cbor::decode(&bytes).expect("decode");
            assert_eq!(back, body);
        }
    }

    #[test]
    fn a_request_without_optional_fields_decodes_with_their_defaults() {
        let value = serde_json::json!({ "PolicyUpsertRequest": { "spec": { "name": "Min" } } });
        let payload: SlaPayload = serde_json::from_value(value).expect("decode");
        let SlaPayload::PolicyUpsertRequest { policy_id, spec } = payload else {
            panic!("wrong variant");
        };
        assert_eq!(policy_id, None);
        assert_eq!(spec.name, "Min");
        assert_eq!(spec.max_concurrency, None);
        assert_eq!(spec.ordering, "partition");

        let value = serde_json::json!({ "BreachListRequest": {} });
        let payload: SlaPayload = serde_json::from_value(value).expect("decode");
        assert_eq!(
            payload,
            SlaPayload::BreachListRequest {
                flow_id: None,
                open_only: false,
                offset: 0,
                limit: 0,
                org_id: None,
            }
        );
    }

    #[test]
    fn a_status_from_a_node_that_knows_fewer_fields_still_decodes() {
        let value = serde_json::json!({ "flow_id": "f-1" });
        let status: SlaFlowStatusWire = serde_json::from_value(value).expect("decode");
        assert_eq!(status.flow_id, "f-1");
        assert_eq!(status.in_flight, 0);
        assert!(status.window.is_none() && status.open_breaches.is_empty());
        assert_eq!(
            (
                status.rejected_calls_today,
                status.rejected_concurrency_today
            ),
            (0, 0)
        );
    }
}
