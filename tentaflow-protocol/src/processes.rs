// ============ File: processes.rs — BPMN process definitions, instances, and typed wire requests ============

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessModel {
    pub schema_version: u32,
    pub process_id: String,
    pub nodes: Vec<ProcessNode>,
    pub sequence_flows: Vec<ProcessSequenceFlow>,
    pub variables: BTreeMap<String, Value>,
    pub diagram: ProcessDiagram,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_timezone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessNode {
    pub id: String,
    pub name: String,
    pub kind: ProcessNodeKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessNodeKind {
    Start,
    End,
    UserTask {
        assignee_user_id: Option<String>,
        output_mapping: BTreeMap<String, String>,
    },
    ServiceTask {
        flow_id: String,
        input_mapping: BTreeMap<String, String>,
        output_mapping: BTreeMap<String, String>,
        verification: ActivityVerification,
        timeout_seconds: u32,
    },
    ExclusiveGateway {
        default_flow_id: Option<String>,
    },
    ParallelGateway,
    TimerStart {
        timer: ProcessTimerSpec,
    },
    TimerCatch {
        timer: ProcessTimerSpec,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ProcessTimerSpec {
    Date { at: String },
    Duration { seconds: u32 },
    Cycle {
        seconds: u32,
        total_firings: Option<u32>,
    },
    Daily {
        hour: u8,
        minute: u8,
        total_firings: Option<u32>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessTimerKind {
    Start,
    Catch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessTimerStatus {
    Pending,
    Fired,
    Cancelled,
    Archived,
    Blocked,
    Missed,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessTimerSummary {
    pub timer_id: String,
    pub node_id: String,
    pub node_name: String,
    pub kind: ProcessTimerKind,
    pub status: ProcessTimerStatus,
    pub due_at_ms: Option<i64>,
    pub timezone: String,
    pub occurrence: u64,
    pub total_firings: Option<u32>,
    pub last_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum ActivityVerification {
    Condition {
        expression: String,
    },
    #[default]
    Human,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessSequenceFlow {
    pub id: String,
    pub source_id: String,
    pub target_id: String,
    pub condition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProcessDiagram {
    pub shapes: Vec<ProcessShape>,
    pub edges: Vec<ProcessEdgeDiagram>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessShape {
    pub element_id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessEdgeDiagram {
    pub sequence_flow_id: String,
    pub waypoints: Vec<ProcessPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessDefinition {
    pub definition_id: String,
    pub name: String,
    pub description: String,
    pub owner_user_id: String,
    pub draft_revision: u64,
    pub model: ProcessModel,
    pub published_version: Option<u32>,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessDefinitionSummary {
    pub definition_id: String,
    pub name: String,
    pub description: String,
    pub owner_user_id: String,
    pub draft_revision: u64,
    pub published_version: Option<u32>,
    pub archived: bool,
}

impl From<&ProcessDefinition> for ProcessDefinitionSummary {
    fn from(value: &ProcessDefinition) -> Self {
        Self {
            definition_id: value.definition_id.clone(),
            name: value.name.clone(),
            description: value.description.clone(),
            owner_user_id: value.owner_user_id.clone(),
            draft_revision: value.draft_revision,
            published_version: value.published_version,
            archived: value.archived,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinnedFlowInfo {
    pub node_id: String,
    pub flow_id: String,
    pub source_version: u32,
    pub graph_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessVersion {
    pub definition_id: String,
    pub version: u32,
    pub model: ProcessModel,
    pub published_at_ms: i64,
    pub published_by: String,
    pub model_sha256: String,
    pub service_flows: Vec<PinnedFlowInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessVersionSummary {
    pub definition_id: String,
    pub version: u32,
    pub published_at_ms: i64,
    pub published_by: String,
    pub model_sha256: String,
}

impl From<&ProcessVersion> for ProcessVersionSummary {
    fn from(value: &ProcessVersion) -> Self {
        Self {
            definition_id: value.definition_id.clone(),
            version: value.version,
            published_at_ms: value.published_at_ms,
            published_by: value.published_by.clone(),
            model_sha256: value.model_sha256.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessInstanceStatus {
    Running,
    Waiting,
    Completed,
    Incident,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessUserTaskKind {
    Work,
    Verification,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessUserTaskStatus {
    Open,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessUserTask {
    pub user_task_id: String,
    pub node_id: String,
    pub name: String,
    pub assignee_user_id: String,
    pub kind: ProcessUserTaskKind,
    pub status: ProcessUserTaskStatus,
    pub outputs: Value,
    pub revision: u64,
    pub can_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessUserTaskSummary {
    pub user_task_id: String,
    pub node_id: String,
    pub name: String,
    pub assignee_user_id: String,
    pub kind: ProcessUserTaskKind,
    pub status: ProcessUserTaskStatus,
    pub revision: u64,
    pub can_complete: bool,
}

impl From<&ProcessUserTask> for ProcessUserTaskSummary {
    fn from(value: &ProcessUserTask) -> Self {
        Self {
            user_task_id: value.user_task_id.clone(),
            node_id: value.node_id.clone(),
            name: value.name.clone(),
            assignee_user_id: value.assignee_user_id.clone(),
            kind: value.kind.clone(),
            status: value.status.clone(),
            revision: value.revision,
            can_complete: value.can_complete,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessIncident {
    pub incident_id: String,
    pub node_id: Option<String>,
    pub node_name: Option<String>,
    pub job_id: Option<String>,
    pub code: String,
    pub message: String,
    pub at_ms: i64,
    pub can_retry: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInstance {
    pub instance_id: String,
    pub definition_id: String,
    pub definition_name: String,
    pub initiator_user_id: String,
    pub version: u32,
    pub revision: u64,
    pub status: ProcessInstanceStatus,
    pub variables: Value,
    pub active_node_ids: Vec<String>,
    pub user_tasks: Vec<ProcessUserTaskSummary>,
    pub incidents: Vec<ProcessIncident>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub can_cancel: bool,
    pub can_retry: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timers: Vec<ProcessTimerSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInstanceSummary {
    pub instance_id: String,
    pub definition_id: String,
    pub definition_name: String,
    pub initiator_user_id: String,
    pub version: u32,
    pub revision: u64,
    pub status: ProcessInstanceStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub can_cancel: bool,
    pub can_retry: bool,
}

impl From<&ProcessInstance> for ProcessInstanceSummary {
    fn from(value: &ProcessInstance) -> Self {
        Self {
            instance_id: value.instance_id.clone(),
            definition_id: value.definition_id.clone(),
            definition_name: value.definition_name.clone(),
            initiator_user_id: value.initiator_user_id.clone(),
            version: value.version,
            revision: value.revision,
            status: value.status.clone(),
            created_at_ms: value.created_at_ms,
            updated_at_ms: value.updated_at_ms,
            can_cancel: value.can_cancel,
            can_retry: value.can_retry,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ActivityOutcome {
    Completed,
    Error,
    NeedsHuman,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityResult {
    pub outcome: ActivityOutcome,
    pub code: Option<String>,
    pub summary: String,
    pub outputs: Value,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessEvent {
    pub event_id: String,
    pub seq: u64,
    pub at_ms: i64,
    pub kind: String,
    pub node_id: Option<String>,
    pub node_name: Option<String>,
    pub actor_user_id: Option<String>,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessDiagnostic {
    pub code: String,
    pub message: String,
    pub element_id: Option<String>,
    pub offset: Option<usize>,
    pub fatal: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessOptionUser {
    pub user_id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessOptionFlow {
    pub flow_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessPayload {
    OptionsRequest {},
    OptionsResponse {
        assignees: Vec<ProcessOptionUser>,
        service_flows: Vec<ProcessOptionFlow>,
    },
    DefinitionListRequest {
        offset: u32,
        limit: u32,
    },
    DefinitionListResponse {
        definitions: Vec<ProcessDefinitionSummary>,
        total: u32,
        has_more: bool,
    },
    DefinitionGetRequest {
        definition_id: String,
    },
    DefinitionGetResponse {
        definition: ProcessDefinition,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timer_start: Option<ProcessTimerSummary>,
    },
    DefinitionSaveRequest {
        command_id: String,
        definition_id: Option<String>,
        expected_revision: u64,
        name: String,
        description: String,
        model: ProcessModel,
    },
    DefinitionSaveResponse {
        definition: ProcessDefinition,
    },
    DefinitionPublishRequest {
        command_id: String,
        definition_id: String,
        expected_revision: u64,
    },
    DefinitionPublishResponse {
        definition: ProcessDefinitionSummary,
        version: ProcessVersion,
    },
    DefinitionArchiveRequest {
        command_id: String,
        definition_id: String,
        expected_revision: u64,
        archived: bool,
    },
    DefinitionArchiveResponse {
        definition: ProcessDefinition,
    },
    VersionListRequest {
        definition_id: String,
        offset: u32,
        limit: u32,
    },
    VersionListResponse {
        versions: Vec<ProcessVersionSummary>,
        total: u32,
        has_more: bool,
    },
    VersionGetRequest {
        definition_id: String,
        version: u32,
    },
    VersionGetResponse {
        version: ProcessVersion,
    },
    XmlImportRequest {
        xml: String,
    },
    XmlImportResponse {
        model: Option<ProcessModel>,
        diagnostics: Vec<ProcessDiagnostic>,
    },
    XmlExportRequest {
        definition_id: String,
        version: Option<u32>,
    },
    XmlExportResponse {
        xml: String,
    },
    InstanceStartRequest {
        command_id: String,
        definition_id: String,
        version: u32,
        variables: Value,
    },
    InstanceStartResponse {
        instance: ProcessInstance,
    },
    InstanceListRequest {
        definition_id: Option<String>,
        offset: u32,
        limit: u32,
    },
    InstanceListResponse {
        instances: Vec<ProcessInstanceSummary>,
        total: u32,
        has_more: bool,
    },
    InstanceGetRequest {
        instance_id: String,
    },
    InstanceGetResponse {
        instance: ProcessInstance,
    },
    UserTaskCompleteRequest {
        command_id: String,
        instance_id: String,
        user_task_id: String,
        expected_revision: u64,
        outputs: Value,
        approved: Option<bool>,
    },
    UserTaskCompleteResponse {
        instance: ProcessInstance,
    },
    InstanceCancelRequest {
        command_id: String,
        instance_id: String,
        expected_revision: u64,
    },
    InstanceCancelResponse {
        instance: ProcessInstance,
    },
    JobRetryRequest {
        command_id: String,
        instance_id: String,
        job_id: String,
        expected_revision: u64,
    },
    JobRetryResponse {
        instance: ProcessInstance,
    },
    HistoryRequest {
        instance_id: String,
        after_seq: u64,
        limit: u32,
    },
    HistoryResponse {
        events: Vec<ProcessEvent>,
        next_seq: u64,
        has_more: bool,
    },
    UserTaskGetRequest {
        instance_id: String,
        user_task_id: String,
    },
    UserTaskGetResponse {
        task: ProcessUserTask,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_rules_round_trip_and_timerless_model_omits_new_fields() {
        let timerless = ProcessModel {
            schema_version: 1, process_id: "P_1".into(), nodes: Vec::new(),
            sequence_flows: Vec::new(), variables: BTreeMap::new(),
            diagram: ProcessDiagram::default(), timer_timezone: None,
        };
        let baseline = serde_json::json!({"schema_version":1,"process_id":"P_1","nodes":[],"sequence_flows":[],"variables":{},"diagram":{"shapes":[],"edges":[]}});
        assert_eq!(serde_json::to_value(&timerless).unwrap(), baseline);
        assert_eq!(serde_json::to_string(&timerless).unwrap(),
            "{\"schema_version\":1,\"process_id\":\"P_1\",\"nodes\":[],\"sequence_flows\":[],\"variables\":{},\"diagram\":{\"shapes\":[],\"edges\":[]}}");
        for (kind, timer) in [
            ("TimerStart", ProcessTimerSpec::Date { at: "2027-01-02T03:04:05+01:00".into() }),
            ("TimerCatch", ProcessTimerSpec::Duration { seconds: 90_061 }),
            ("TimerStart", ProcessTimerSpec::Cycle { seconds: 300, total_firings: Some(3) }),
            ("TimerStart", ProcessTimerSpec::Daily { hour: 9, minute: 15, total_firings: None }),
        ] {
            let node_kind = if kind == "TimerCatch" { ProcessNodeKind::TimerCatch { timer } } else { ProcessNodeKind::TimerStart { timer } };
            let mut model = timerless.clone();
            model.timer_timezone = Some("Europe/Warsaw".into());
            model.nodes.push(ProcessNode { id: "Start_1".into(), name: "Start".into(), kind: node_kind });
            let bytes = crate::cbor::encode(&model).unwrap();
            let decoded: ProcessModel = crate::cbor::decode(&bytes).unwrap();
            assert_eq!(decoded, model);
        }
        assert!(serde_json::from_value::<ProcessTimerSpec>(serde_json::json!({"Cycle":{"seconds":300,"total_firings":3,"unknown":true}})).is_err());
        assert!(serde_json::from_value::<ProcessModel>(serde_json::json!({"schema_version":1,"process_id":"P_1","nodes":[],"sequence_flows":[],"variables":{},"diagram":{"shapes":[],"edges":[]},"timer_timezone":null,"unknown":true})).is_err());
    }

    #[test]
    fn process_payload_round_trip_preserves_nested_model_and_tag() {
        let payload = ProcessPayload::DefinitionSaveRequest {
            command_id: "77b1c94f-6f69-4b71-a344-97caa34cb2c0".into(),
            definition_id: None,
            expected_revision: 0,
            name: "Approval".into(),
            description: String::new(),
            model: ProcessModel {
                schema_version: 1,
                process_id: "Process_1".into(),
                nodes: vec![ProcessNode {
                    id: "Start_1".into(),
                    name: "Start".into(),
                    kind: ProcessNodeKind::Start,
                }],
                sequence_flows: Vec::new(),
                variables: BTreeMap::new(),
                diagram: ProcessDiagram::default(),
                timer_timezone: None,
            },
        };
        let bytes = crate::cbor::encode(&payload).unwrap();
        let decoded: ProcessPayload = crate::cbor::decode(&bytes).unwrap();
        assert_eq!(decoded, payload);
        let json = serde_json::to_value(&decoded).unwrap();
        assert!(json.get("DefinitionSaveRequest").is_some());
    }

    #[test]
    fn process_body_tag_and_paged_summary_shape_are_stable() {
        let body =
            crate::message_body::MessageBody::ProcessBody(ProcessPayload::DefinitionListResponse {
                definitions: vec![ProcessDefinitionSummary {
                    definition_id: "d".into(),
                    name: "Decision".into(),
                    description: String::new(),
                    owner_user_id: "u".into(),
                    draft_revision: 2,
                    published_version: Some(1),
                    archived: false,
                }],
                total: 1,
                has_more: false,
            });
        let encoded = crate::cbor::encode(&body).unwrap();
        let decoded: crate::message_body::MessageBody = crate::cbor::decode(&encoded).unwrap();
        assert_eq!(decoded, body);
        let golden = serde_json::json!({"ProcessBody":{"DefinitionListResponse":{
            "definitions":[{"definition_id":"d","name":"Decision","description":"",
                "owner_user_id":"u","draft_revision":2,"published_version":1,"archived":false}],
            "total":1,"has_more":false
        }}});
        assert_eq!(serde_json::to_value(decoded).unwrap(), golden);
    }

    #[test]
    fn user_task_summary_omits_outputs_while_detail_preserves_them() {
        let task = ProcessUserTask {
            user_task_id: "task-1".into(),
            node_id: "Review_1".into(),
            name: "Review".into(),
            assignee_user_id: "user-1".into(),
            kind: ProcessUserTaskKind::Work,
            status: ProcessUserTaskStatus::Open,
            outputs: serde_json::json!({"business_key": {"inner_value": 3}}),
            revision: 1,
            can_complete: true,
        };
        let summary = ProcessUserTaskSummary::from(&task);
        let summary_json = serde_json::to_value(&summary).unwrap();
        assert!(summary_json.get("outputs").is_none());
        let payload = ProcessPayload::UserTaskGetResponse { task: task.clone() };
        let encoded = crate::cbor::encode(&payload).unwrap();
        let decoded: ProcessPayload = crate::cbor::decode(&encoded).unwrap();
        assert_eq!(decoded, payload);
        let detail = serde_json::to_value(decoded).unwrap();
        assert_eq!(
            detail["UserTaskGetResponse"]["task"]["outputs"]["business_key"]["inner_value"],
            3
        );
    }
}
