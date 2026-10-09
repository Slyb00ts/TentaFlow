// ============ File: processes.rs — BPMN process definitions, instances, and typed wire requests ============

use std::collections::BTreeMap;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeStruct;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_calendar: Option<ProcessWorkCalendar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_pin: Option<ProcessCalendarPin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<ProcessMessageDeclaration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ProcessErrorDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalations: Vec<ProcessEscalationDeclaration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<ProcessSignalDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_processes: Vec<ProcessExecutableProcess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modeling: Option<ProcessBodyModeling>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collaboration: Option<ProcessCollaboration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_stores: Vec<ProcessDataStore>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessExecutableProcess {
    pub process_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
    pub nodes: Vec<ProcessNode>,
    pub sequence_flows: Vec<ProcessSequenceFlow>,
    pub variables: BTreeMap<String, Value>,
    pub diagram: ProcessDiagram,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_calendar: Option<ProcessWorkCalendar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_pin: Option<ProcessCalendarPin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modeling: Option<ProcessBodyModeling>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProcessBodyModeling {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lane_sets: Vec<ProcessLaneSet>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_objects: Vec<ProcessDataObject>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_object_references: Vec<ProcessDataObjectReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_annotations: Vec<ProcessTextAnnotation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub associations: Vec<ProcessAssociation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_store_references: Vec<ProcessDataStoreReference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessLaneSet {
    pub id: String,
    pub lanes: Vec<ProcessLane>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessLane {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub flow_node_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_lane_sets: Vec<ProcessLaneSet>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessDataObject {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessDataObjectReference {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub data_object_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variable_binding_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessDataStore {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_unlimited: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessDataStoreReference {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub data_store_ref: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessTextAnnotation {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessAssociation {
    pub id: String,
    pub source_ref: String,
    pub target_ref: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessCollaboration {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub participants: Vec<ProcessParticipant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_flows: Vec<ProcessMessageFlow>,
    pub diagram: ProcessDiagram,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessParticipant {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_ref: Option<ProcessCallableReference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessMessageFlow {
    pub id: String,
    pub source_ref: String,
    pub target_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessMessageDeclaration {
    pub message_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessErrorDeclaration {
    pub error_id: String,
    pub name: String,
    pub error_code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessEscalationDeclaration {
    pub escalation_id: String,
    pub name: String,
    pub escalation_code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSignalDeclaration {
    pub signal_id: String,
    pub namespace_uri: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ProcessMessageTargetSpec {
    Start {
        definition_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_node_id: Option<String>,
    },
    Catch {
        definition_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instance_id_expression: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subscription_id_expression: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessWorkCalendar {
    pub name: String,
    pub weekly_windows: Vec<WorkWindow>,
    pub manual_days_off: Vec<ManualDayOff>,
    pub holiday_policy: HolidayPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkWindow {
    pub weekday: u8,
    pub start_minute: u16,
    pub end_minute: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualDayOff {
    pub date: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HolidayPolicy {
    PolandStatutory,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessCalendarPin {
    pub calendar: ProcessWorkCalendar,
    pub legal_release: ProcessLegalRelease,
    pub timezone_data: ProcessTimezoneData,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessLegalRelease {
    pub release_id: String,
    pub as_of_date: String,
    pub valid_from: String,
    pub valid_until: String,
    pub audit_manifest_sha256: String,
    pub sources: Vec<ProcessCalendarSource>,
    pub rules: Vec<ProcessHolidayRule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessCalendarSource {
    pub source_id: String,
    pub url: String,
    pub sha256: String,
    pub retrieved_on: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessHolidayRule {
    pub rule_id: String,
    pub source_id: String,
    pub effective_from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_until: Option<String>,
    pub kind: ProcessHolidayRuleKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ProcessHolidayRuleKind {
    Fixed { month: u8, day: u8 },
    GregorianEasterOffset { days: i16 },
    Weekday { weekday: u8 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessTimezoneData {
    pub iana_name: String,
    pub release_id: String,
    pub horizon_start_ms: i64,
    pub horizon_end_ms: i64,
    pub initial_offset_seconds: i32,
    pub transitions: Vec<ProcessOffsetTransition>,
    pub source_url: String,
    pub source_sha256: String,
    pub dataset_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessOffsetTransition {
    pub at_utc_ms: i64,
    pub offset_seconds: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessCalendarPinState {
    Unpinned,
    Current,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessNode {
    pub id: String,
    pub name: String,
    pub kind: ProcessNodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<ProcessRepeatSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_io: Option<ProcessActivityIo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessActivityIo {
    pub data_inputs: Vec<ProcessIoDataInput>,
    pub data_outputs: Vec<ProcessIoDataOutput>,
    pub input_set_id: String,
    pub input_set: Vec<String>,
    pub output_set_id: String,
    pub output_set: Vec<String>,
    pub input_associations: Vec<ProcessInputAssociation>,
    pub output_associations: Vec<ProcessOutputAssociation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_output: Option<ProcessCoordinatorOutputIo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessCoordinatorOutputIo {
    pub data_outputs: Vec<ProcessIoDataOutput>,
    pub output_set_id: String,
    pub output_set: Vec<String>,
    pub output_associations: Vec<ProcessOutputAssociation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIoDataInput {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIoDataOutput {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub value_expression: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ProcessInputAssociation {
    DirectRef {
        id: String,
        source_object_ref_id: String,
        target_input_id: String,
    },
    CelAssignment {
        id: String,
        from_expression: String,
        target_input_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessOutputAssociation {
    pub id: String,
    pub source_output_id: String,
    pub target_object_ref_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ProcessRepeatSpec {
    MultiInstance {
        mode: ProcessMultiInstanceMode,
        input: ProcessMultiInstanceInput,
        output_collection_variable: String,
    },
    StructuredLoop {
        condition: String,
        test_before: bool,
        max_iterations: u8,
        output_collection_variable: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessMultiInstanceMode {
    Sequential,
    Parallel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ProcessMultiInstanceInput {
    Cardinality { count: u8 },
    CollectionExpression { expression: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSubProcess {
    pub nodes: Vec<ProcessNode>,
    pub sequence_flows: Vec<ProcessSequenceFlow>,
    pub variables: BTreeMap<String, Value>,
    pub diagram: ProcessDiagram,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modeling: Option<ProcessBodyModeling>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessCallableReference {
    pub namespace_uri: String,
    pub process_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProcessCallTarget {
    PublishedBody {
        definition_id: String,
        version: u32,
        called_element: ProcessCallableReference,
    },
    LocalBody {
        called_element: ProcessCallableReference,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessCallActivity {
    pub target: ProcessCallTarget,
    pub input_mapping: BTreeMap<String, String>,
    pub output_mapping: BTreeMap<String, String>,
}

impl Serialize for ProcessCallActivity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.target {
            ProcessCallTarget::PublishedBody {
                definition_id,
                version,
                called_element,
            } => {
                let mut call = serializer.serialize_struct("ProcessCallActivity", 5)?;
                call.serialize_field("called_definition_id", definition_id)?;
                call.serialize_field("called_version", version)?;
                call.serialize_field("called_element", called_element)?;
                call.serialize_field("input_mapping", &self.input_mapping)?;
                call.serialize_field("output_mapping", &self.output_mapping)?;
                call.end()
            }
            ProcessCallTarget::LocalBody { called_element } => {
                let mut call = serializer.serialize_struct("ProcessCallActivity", 3)?;
                call.serialize_field("local_body", called_element)?;
                call.serialize_field("input_mapping", &self.input_mapping)?;
                call.serialize_field("output_mapping", &self.output_mapping)?;
                call.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ProcessCallActivity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct CallVisitor;

        impl<'de> Visitor<'de> for CallVisitor {
            type Value = ProcessCallActivity;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("one published or local CallActivity target")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut definition_id: Option<String> = None;
                let mut version: Option<u32> = None;
                let mut called_element: Option<ProcessCallableReference> = None;
                let mut local_body: Option<ProcessCallableReference> = None;
                let mut input_mapping: Option<BTreeMap<String, String>> = None;
                let mut output_mapping: Option<BTreeMap<String, String>> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "called_definition_id" => {
                            if definition_id.replace(map.next_value()?).is_some() {
                                return Err(de::Error::duplicate_field("called_definition_id"));
                            }
                        }
                        "called_version" => {
                            if version.replace(map.next_value()?).is_some() {
                                return Err(de::Error::duplicate_field("called_version"));
                            }
                        }
                        "called_element" => {
                            if called_element.replace(map.next_value()?).is_some() {
                                return Err(de::Error::duplicate_field("called_element"));
                            }
                        }
                        "local_body" => {
                            if local_body.replace(map.next_value()?).is_some() {
                                return Err(de::Error::duplicate_field("local_body"));
                            }
                        }
                        "input_mapping" => {
                            if input_mapping.replace(map.next_value()?).is_some() {
                                return Err(de::Error::duplicate_field("input_mapping"));
                            }
                        }
                        "output_mapping" => {
                            if output_mapping.replace(map.next_value()?).is_some() {
                                return Err(de::Error::duplicate_field("output_mapping"));
                            }
                        }
                        _ => return Err(de::Error::unknown_field(&key, &[
                            "called_definition_id", "called_version", "called_element",
                            "local_body", "input_mapping", "output_mapping",
                        ])),
                    }
                }
                let target = if let Some(local_reference) = local_body {
                    if definition_id.is_some() || version.is_some() || called_element.is_some() {
                        return Err(de::Error::custom("local and published Call targets are mixed"));
                    }
                    ProcessCallTarget::LocalBody { called_element: local_reference }
                } else {
                    let definition_id = definition_id
                        .ok_or_else(|| de::Error::missing_field("called_definition_id"))?;
                    let version = version
                        .ok_or_else(|| de::Error::missing_field("called_version"))?;
                    if version == 0 {
                        return Err(de::Error::custom("published Call version must be positive"));
                    }
                    let called_element = called_element
                        .ok_or_else(|| de::Error::missing_field("called_element"))?;
                    ProcessCallTarget::PublishedBody {
                        definition_id,
                        version,
                        called_element,
                    }
                };
                Ok(ProcessCallActivity {
                    target,
                    input_mapping: input_mapping
                        .ok_or_else(|| de::Error::missing_field("input_mapping"))?,
                    output_mapping: output_mapping
                        .ok_or_else(|| de::Error::missing_field("output_mapping"))?,
                })
            }
        }

        deserializer.deserialize_map(CallVisitor)
    }
}

fn deserialize_manual_assignee<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_expression: Option<String>,
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
    BoundaryTimer {
        attached_to_id: String,
        cancel_activity: bool,
        timer: ProcessTimerSpec,
    },
    MessageStart {
        message_ref: String,
        output_mapping: BTreeMap<String, String>,
    },
    MessageCatch {
        message_ref: String,
        correlation_expression: String,
        output_mapping: BTreeMap<String, String>,
    },
    MessageThrow {
        message_ref: String,
        target: ProcessMessageTargetSpec,
        correlation_expression: String,
        payload_expression: String,
        ttl_seconds: u32,
    },
    BoundaryMessage {
        attached_to_id: String,
        cancel_activity: bool,
        message_ref: String,
        correlation_expression: String,
        output_mapping: BTreeMap<String, String>,
    },
    EventBasedGateway,
    BoundaryError {
        attached_to_id: String,
        error_ref: Option<String>,
        output_mapping: BTreeMap<String, String>,
    },
    SubProcess {
        body: ProcessSubProcess,
        input_mapping: BTreeMap<String, String>,
        output_mapping: BTreeMap<String, String>,
    },
    CallActivity(ProcessCallActivity),
    ErrorEnd {
        error_ref: String,
    },
    InclusiveGateway {
        default_flow_id: Option<String>,
    },
    BoundaryEscalation {
        attached_to_id: String,
        escalation_ref: Option<String>,
        cancel_activity: bool,
        output_mapping: BTreeMap<String, String>,
    },
    TerminateEnd,
    ScriptTask {
        script: String,
        output_mapping: BTreeMap<String, String>,
    },
    ManualTask {
        #[serde(deserialize_with = "deserialize_manual_assignee")]
        assignee_user_id: Option<String>,
        instructions: String,
    },
    SendTask {
        message_ref: String,
        target: ProcessMessageTargetSpec,
        correlation_expression: String,
        payload_expression: String,
        ttl_seconds: u32,
    },
    ReceiveTask {
        message_ref: String,
        correlation_expression: String,
        output_mapping: BTreeMap<String, String>,
    },
    SignalThrow {
        signal_ref: String,
        payload_expression: String,
        ttl_seconds: u32,
    },
    SignalCatch {
        signal_ref: String,
        output_mapping: BTreeMap<String, String>,
    },
    LinkThrow {
        definition: ProcessLinkEventDefinition,
    },
    LinkCatch {
        definition: ProcessLinkEventDefinition,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessLinkEventDefinition {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_ref: Option<String>,
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
    WorkingDuration { seconds: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessTimerKind {
    Start,
    Catch,
    Boundary,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached_to_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_time: Option<ProcessWorkingTimeSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessWorkingTimeSummary {
    pub calendar_name: String,
    pub holiday_policy: HolidayPolicy,
    pub pin_sha256: String,
    pub legal_release_id: String,
    pub legal_as_of_date: String,
    pub tzdb_release_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_offset_seconds: Option<i32>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_start_node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProcessDiagram {
    pub shapes: Vec<ProcessShape>,
    pub edges: Vec<ProcessEdgeDiagram>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modeling_shapes: Vec<ProcessModelingShape>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modeling_edges: Vec<ProcessModelingEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessModelingShape {
    pub di_id: String,
    pub element_id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessModelingEdge {
    pub di_id: String,
    pub element_id: String,
    pub waypoints: Vec<ProcessPoint>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_pin_state: Option<ProcessCalendarPinState>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_pin_state: Option<ProcessCalendarPinState>,
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
            calendar_pin_state: value.calendar_pin_state.clone(),
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
#[serde(deny_unknown_fields)]
pub struct ProcessCallPinInfo {
    pub node_id: String,
    pub called_definition_id: String,
    pub called_version: u32,
    pub called_element: ProcessCallableReference,
    pub model_sha256: String,
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
    pub call_activities: Vec<ProcessCallPinInfo>,
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
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessTerminalError {
    pub error_ref: String,
    pub error_code: String,
    pub source_event_id: String,
    pub source_node_id: String,
    pub source_scope_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessCallStatus {
    Waiting,
    Returned,
    Error,
    Cancelled,
    ReturnIncident,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRelatedInstance {
    pub instance_id: String,
    pub definition_name: String,
    pub version: u32,
    pub status: ProcessInstanceStatus,
    pub can_open: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ProcessCallSummary {
    Outgoing {
        call_node_id: String,
        call_node_name: String,
        status: ProcessCallStatus,
        child: Option<ProcessRelatedInstance>,
    },
    Incoming {
        parent: Option<ProcessRelatedInstance>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessUserTaskKind {
    Work,
    Verification,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessUserTaskStatus {
    Open,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessUserTaskInputValue {
    Missing,
    Present(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessUserTaskInput {
    pub position: u16,
    pub declaration_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub value: ProcessUserTaskInputValue,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    pub scope_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activity_inputs: Vec<ProcessUserTaskInput>,
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
    pub scope_id: String,
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
            scope_id: value.scope_id.clone(),
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
    pub scope_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ProcessMessageTarget {
    Start {
        definition_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_node_id: Option<String>,
    },
    Catch {
        definition_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instance_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subscription_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessMessageStatus {
    Pending,
    Blocked,
    Ambiguous,
    Delivered,
    Expired,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessMessageOrigin {
    Api,
    Process,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessSubscriptionKind {
    MessageCatch,
    BoundaryMessage,
    BoundaryError,
    BoundaryEscalation,
    ReceiveTask,
    SignalCatch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessSubscriptionStatus {
    Open,
    Consumed,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcessEventRaceStatus {
    Open,
    Won,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessMessageSummary {
    pub message_id: String,
    pub sender_user_id: String,
    pub origin: ProcessMessageOrigin,
    pub target: ProcessMessageTarget,
    pub message_name: String,
    pub correlation_key: String,
    pub revision: u64,
    pub status: ProcessMessageStatus,
    pub received_at_ms: i64,
    pub expires_at_ms: i64,
    pub updated_at_ms: i64,
    pub delivered_at_ms: Option<i64>,
    pub matched_instance_id: Option<String>,
    pub matched_version: Option<u32>,
    pub matched_subscription_id: Option<String>,
    pub source_instance_id: Option<String>,
    pub source_node_id: Option<String>,
    pub last_reason: Option<String>,
    pub payload_sha256: String,
    pub payload_bytes: u32,
    pub payload_available: bool,
    pub can_resolve: bool,
    pub can_cancel: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_scope_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessMessageDetail {
    pub message: ProcessMessageSummary,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_present_message_payload")]
    pub payload: Option<Value>,
}

fn deserialize_present_message_payload<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessPageSpec {
    pub offset: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessPageInfo {
    pub offset: u32,
    pub total: u32,
    pub next_offset: Option<u32>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessInstancePageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_tasks: Option<ProcessPageSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incidents: Option<ProcessPageSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timers: Option<ProcessPageSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscriptions: Option<ProcessPageSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_races: Option<ProcessPageSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing_messages: Option<ProcessPageSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_user_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_incident_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<ProcessPageSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<ProcessPageSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repetition_groups: Option<ProcessPageSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repetition_occurrences: Option<ProcessPageSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_repetition_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_repetition_occurrence_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_repetition_value: Option<ProcessRepetitionValueKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessInstancePageInfo {
    pub user_tasks: ProcessPageInfo,
    pub incidents: ProcessPageInfo,
    pub timers: ProcessPageInfo,
    pub subscriptions: ProcessPageInfo,
    pub event_races: ProcessPageInfo,
    pub outgoing_messages: ProcessPageInfo,
    pub scopes: ProcessPageInfo,
    pub calls: ProcessPageInfo,
    pub repetition_groups: ProcessPageInfo,
    pub repetition_occurrences: ProcessPageInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessRepetitionGroupMode {
    MultiInstanceSequential,
    MultiInstanceParallel,
    StructuredLoop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessRepetitionGroupStatus {
    Open,
    Incident,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessRepetitionOccurrenceStatus {
    Pending,
    Active,
    AwaitingVerification,
    RetryableError,
    AcceptedBlocked,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessRepetitionValueKind {
    Item,
    Aggregate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessActivityResultOrigin {
    Envelope,
    Contract,
    Platform,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRepetitionGroupSummary {
    pub group_id: String,
    pub node_id: String,
    pub node_name: String,
    pub scope_id: String,
    pub parent_token_id: String,
    pub mode: ProcessRepetitionGroupMode,
    pub status: ProcessRepetitionGroupStatus,
    pub total: Option<u32>,
    pub created_count: u32,
    pub completed: u32,
    pub max_iterations: Option<u8>,
    pub revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRepetitionOccurrenceSummary {
    pub occurrence_id: String,
    pub group_id: String,
    pub ordinal: u32,
    pub status: ProcessRepetitionOccurrenceStatus,
    pub token_id: String,
    pub user_task_id: Option<String>,
    pub job_id: Option<String>,
    pub verification_user_task_id: Option<String>,
    pub accepted_source_event_id: Option<String>,
    pub approval_event_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRepetitionOccurrenceDetail {
    pub summary: ProcessRepetitionOccurrenceSummary,
    pub value_kind: ProcessRepetitionValueKind,
    pub value_available: bool,
    pub value: Value,
    pub accepted_origin: Option<ProcessActivityResultOrigin>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessScopeSummary {
    pub scope_id: String,
    pub parent_scope_id: Option<String>,
    pub subprocess_node_id: Option<String>,
    pub subprocess_node_name: Option<String>,
    pub parent_token_id: Option<String>,
    pub revision: u64,
    pub status: ProcessInstanceStatus,
    pub depth: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_error: Option<ProcessTerminalError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIncidentSelection {
    pub incident: ProcessIncident,
    pub resolved_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessMessageStartSummary {
    pub node_id: String,
    pub node_name: String,
    pub message_name: String,
    pub version: u32,
    pub can_send: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProcessStartCatalogEntry {
    pub process_id: String,
    pub process_name: Option<String>,
    pub start_node_id: String,
    pub start_node_name: String,
    pub version: u32,
    pub trigger: ProcessStartTrigger,
}

#[derive(Deserialize)]
struct RequiredNullable<T>(Option<T>);

impl<'de> Deserialize<'de> for ProcessStartCatalogEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            process_id: String,
            process_name: RequiredNullable<String>,
            start_node_id: String,
            start_node_name: String,
            version: u32,
            trigger: ProcessStartTrigger,
        }
        let value = Wire::deserialize(deserializer)?;
        Ok(Self {
            process_id: value.process_id,
            process_name: value.process_name.0,
            start_node_id: value.start_node_id,
            start_node_name: value.start_node_name,
            version: value.version,
            trigger: value.trigger,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ProcessStartTrigger {
    Start { can_start: bool },
    MessageStart {
        message_ref: String,
        message_name: String,
        can_send: bool,
    },
    TimerStart {
        timer: ProcessTimerSpec,
        timezone: String,
        working_time: Option<ProcessWorkingTimeSummary>,
        persisted_timer: Option<ProcessTimerSummary>,
    },
}

impl<'de> Deserialize<'de> for ProcessStartTrigger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        enum Wire {
            Start { can_start: bool },
            MessageStart {
                message_ref: String,
                message_name: String,
                can_send: bool,
            },
            TimerStart {
                timer: ProcessTimerSpec,
                timezone: String,
                working_time: RequiredNullable<ProcessWorkingTimeSummary>,
                persisted_timer: RequiredNullable<ProcessTimerSummary>,
            },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Start { can_start } => Self::Start { can_start },
            Wire::MessageStart { message_ref, message_name, can_send } =>
                Self::MessageStart { message_ref, message_name, can_send },
            Wire::TimerStart { timer, timezone, working_time, persisted_timer } =>
                Self::TimerStart {
                    timer,
                    timezone,
                    working_time: working_time.0,
                    persisted_timer: persisted_timer.0,
                },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSubscriptionSummary {
    pub subscription_id: String,
    pub node_id: String,
    pub node_name: String,
    pub token_id: String,
    pub kind: ProcessSubscriptionKind,
    pub status: ProcessSubscriptionStatus,
    pub revision: u64,
    pub message_name: Option<String>,
    pub correlation_key: Option<String>,
    pub error_code: Option<String>,
    pub attached_to_id: Option<String>,
    pub race_id: Option<String>,
    pub last_reason: Option<String>,
    pub scope_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessEventRaceSummary {
    pub race_id: String,
    pub gateway_node_id: String,
    pub gateway_name: String,
    pub status: ProcessEventRaceStatus,
    pub revision: u64,
    pub winner_node_id: Option<String>,
    pub branch_subscription_ids: Vec<String>,
    pub branch_timer_ids: Vec<String>,
    pub scope_id: String,
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
    #[serde(default)]
    pub subscriptions: Vec<ProcessSubscriptionSummary>,
    #[serde(default)]
    pub event_races: Vec<ProcessEventRaceSummary>,
    #[serde(default)]
    pub outgoing_messages: Vec<ProcessMessageSummary>,
    #[serde(default)]
    pub message_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_send_message: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<ProcessInstancePageInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_user_task: Option<ProcessUserTaskSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_incident: Option<ProcessIncidentSelection>,
    #[serde(default)]
    pub scopes: Vec<ProcessScopeSummary>,
    pub calls: Vec<ProcessCallSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_error: Option<ProcessTerminalError>,
    pub repetition_groups: Vec<ProcessRepetitionGroupSummary>,
    pub repetition_occurrences: Vec<ProcessRepetitionOccurrenceSummary>,
    pub selected_repetition_occurrence: Option<ProcessRepetitionOccurrenceDetail>,
    pub process_id: String,
    pub start_node_id: String,
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
    pub process_id: String,
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
            process_id: value.process_id.clone(),
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
    pub scope_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessDiagnostic {
    pub code: String,
    pub message: String,
    pub element_id: Option<String>,
    pub offset: Option<usize>,
    pub fatal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ProcessEscalationPathReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessEscalationPathReason {
    CrossBody,
    ScopeEntry,
    CallEntry,
    TerminalBeforeWait,
    VariableWrite,
    InvalidGateway,
    NoDurableWait,
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
#[serde(deny_unknown_fields)]
pub struct ProcessSimulationSource {
    pub simulation_id: String,
    pub definition_id: String,
    pub version: u32,
    pub model_sha256: String,
    pub selected_process_id: String,
    pub start_node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSimulationClock {
    pub start_ms: i64,
    pub now_ms: i64,
    pub horizon_ms: i64,
    pub tick_duration_ms: i64,
    pub step_index: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSimulationEvent {
    pub event_id: String,
    pub seq: u64,
    pub at_ms: i64,
    pub kind: String,
    pub node_id: Option<String>,
    pub actor_user_id: Option<String>,
    pub data: Value,
    pub scope_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSimulationTraceStep {
    pub trace_step_id: String,
    pub ordinal: u64,
    pub action: String,
    pub at_ms: i64,
    pub request_sha256: String,
    pub result_sha256: String,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSimulationView {
    pub simulation_id: String,
    pub source: ProcessSimulationSource,
    pub clock: ProcessSimulationClock,
    pub instance: Option<ProcessInstance>,
    pub user_tasks: Vec<ProcessUserTask>,
    pub timers: Vec<ProcessTimerSummary>,
    pub incidents: Vec<ProcessIncident>,
    pub events: Vec<ProcessSimulationEvent>,
    pub trace_steps: Vec<ProcessSimulationTraceStep>,
    pub activity_io_witnesses: Vec<Value>,
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
        start_catalog: Vec<ProcessStartCatalogEntry>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repin_calendar: Option<bool>,
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
        start_catalog: Vec<ProcessStartCatalogEntry>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_node_id: Option<String>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pages: Option<ProcessInstancePageRequest>,
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
    MessageSendRequest {
        command_id: String,
        message_id: String,
        target: ProcessMessageTarget,
        message_name: String,
        correlation_key: String,
        payload: Value,
        ttl_seconds: u32,
    },
    MessageSendResponse {
        message: ProcessMessageSummary,
    },
    MessageGetRequest {
        sender_user_id: String,
        message_id: String,
    },
    MessageGetResponse {
        message: ProcessMessageDetail,
    },
    MessageListRequest {
        definition_id: Option<String>,
        instance_id: Option<String>,
        offset: u32,
        limit: u32,
    },
    MessageListResponse {
        messages: Vec<ProcessMessageSummary>,
        total: u32,
        has_more: bool,
    },
    MessageResolveRequest {
        command_id: String,
        message_id: String,
        expected_revision: u64,
        instance_id: String,
        subscription_id: String,
    },
    MessageResolveResponse {
        message: ProcessMessageSummary,
    },
    MessageCancelRequest {
        command_id: String,
        message_id: String,
        expected_revision: u64,
    },
    MessageCancelResponse {
        message: ProcessMessageSummary,
    },
    ScopeGetRequest {
        instance_id: String,
        scope_id: String,
    },
    ScopeGetResponse {
        scope: ProcessScopeSummary,
        variables: Value,
        active_node_ids: Vec<String>,
    },
    ManualTaskAcknowledgeRequest {
        command_id: String,
        instance_id: String,
        user_task_id: String,
        expected_revision: u64,
    },
    ManualTaskAcknowledgeResponse {
        instance: ProcessInstance,
    },
    SimulationStartRequest {
        definition_id: String,
        version: u32,
        selected_process_id: String,
        start_node_id: String,
        variables: Value,
        start_ms: i64,
        horizon_ms: i64,
        tick_duration_ms: i64,
    },
    SimulationStartResponse {
        view: ProcessSimulationView,
    },
    SimulationViewRequest {
        simulation_id: String,
    },
    SimulationViewResponse {
        view: ProcessSimulationView,
    },
    SimulationAdvanceRequest {
        simulation_id: String,
    },
    SimulationAdvanceResponse {
        view: ProcessSimulationView,
    },
    SimulationUserTaskCompleteRequest {
        simulation_id: String,
        user_task_id: String,
        outputs: Value,
    },
    SimulationUserTaskCompleteResponse {
        view: ProcessSimulationView,
    },
    SimulationManualTaskAcknowledgeRequest {
        simulation_id: String,
        user_task_id: String,
    },
    SimulationManualTaskAcknowledgeResponse {
        view: ProcessSimulationView,
    },
    SimulationReleaseRequest {
        simulation_id: String,
    },
    SimulationReleaseResponse {
        simulation_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_model_and_public_summary_round_trip_without_changing_absent_nodes() {
        let old = serde_json::json!({"id":"Review_1","name":"Review","kind":{"UserTask":{
            "assignee_user_id":null,"output_mapping":{}}}});
        let mut node: ProcessNode = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(&node).unwrap(), old);
        node.repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::CollectionExpression { expression: "vars.items".into() },
            output_collection_variable: "results".into(),
        });
        let bytes = crate::cbor::encode(&node).unwrap();
        assert_eq!(crate::cbor::decode::<ProcessNode>(&bytes).unwrap(), node);
        assert_eq!(serde_json::to_value(&node).unwrap()["repeat"]["MultiInstance"]["input"]
            ["CollectionExpression"]["expression"], "vars.items");
        node.repeat = Some(ProcessRepeatSpec::StructuredLoop {
            condition: "vars.again".into(), test_before: false, max_iterations: 32,
            output_collection_variable: "results".into(),
        });
        assert_eq!(crate::cbor::decode::<ProcessNode>(&crate::cbor::encode(&node).unwrap()).unwrap(), node);
        assert!(serde_json::from_value::<ProcessRepeatSpec>(serde_json::json!({"StructuredLoop":{
            "condition":"true","test_before":true,"max_iterations":2,
            "output_collection_variable":"results","unexpected":true}})).is_err());

        let group = ProcessRepetitionGroupSummary { group_id: "g".into(), node_id: "Review_1".into(),
            node_name: "Review".into(), scope_id: "s".into(), parent_token_id: "t".into(),
            mode: ProcessRepetitionGroupMode::StructuredLoop, status: ProcessRepetitionGroupStatus::Open,
            total: None, created_count: 1, completed: 0, max_iterations: Some(32), revision: 1,
            created_at_ms: 1, updated_at_ms: 1 };
        assert_eq!(serde_json::to_value(&group).unwrap()["total"], serde_json::Value::Null);
        assert_eq!(crate::cbor::decode::<ProcessRepetitionGroupSummary>(&crate::cbor::encode(&group).unwrap()).unwrap(), group);
        let detail = ProcessRepetitionOccurrenceDetail { summary: ProcessRepetitionOccurrenceSummary {
            occurrence_id: "o".into(), group_id: "g".into(), ordinal: 0,
            status: ProcessRepetitionOccurrenceStatus::Completed, token_id: "t".into(),
            user_task_id: None, job_id: None, verification_user_task_id: None,
            accepted_source_event_id: Some("e".into()), approval_event_id: None,
            created_at_ms: 1, updated_at_ms: 2 },
            value_kind: ProcessRepetitionValueKind::Aggregate, value_available: true,
            value: serde_json::Value::Null, accepted_origin: Some(ProcessActivityResultOrigin::Envelope),
        };
        assert_eq!(crate::cbor::decode::<ProcessRepetitionOccurrenceDetail>(&crate::cbor::encode(&detail).unwrap()).unwrap(), detail);
        assert!(detail.value_available && detail.value.is_null());
    }

    #[test]
    fn escalation_fields_round_trip_without_changing_absent_model_or_diagnostic_bytes() {
        let old = serde_json::json!({"schema_version":1,"process_id":"P_1","nodes":[],
            "sequence_flows":[],"variables":{},"diagram":{"shapes":[],"edges":[]}});
        let mut model: ProcessModel = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(&model).unwrap(), old);
        model.escalations.push(ProcessEscalationDeclaration {
            escalation_id: "Esc_1".into(),
            name: "Review & approve".into(),
            escalation_code: "NEEDS.HUMAN".into(),
        });
        model.nodes.push(ProcessNode { repeat: None, activity_io: None,
            id: "Boundary_1".into(),
            name: "Review".into(),
            kind: ProcessNodeKind::BoundaryEscalation {
                attached_to_id: "Service_1".into(),
                escalation_ref: Some("Esc_1".into()),
                cancel_activity: false,
                output_mapping: BTreeMap::from([(
                    "business_key".into(),
                    "outputs.customer_ID".into(),
                )]),
            },
        });
        assert_eq!(
            crate::cbor::decode::<ProcessModel>(&crate::cbor::encode(&model).unwrap()).unwrap(),
            model
        );
        assert_eq!(
            serde_json::to_value(&model).unwrap()["nodes"][0]["kind"]["BoundaryEscalation"]
                ["output_mapping"]["business_key"],
            "outputs.customer_ID"
        );
        assert!(serde_json::from_value::<ProcessNodeKind>(
            serde_json::json!({"BoundaryEscalation": {
                "attached_to_id":"Service_1","escalation_ref":null,"cancel_activity":true,
                "output_mapping":{},"unknown":true
            }})
        )
        .is_err());
        let old_diagnostic = serde_json::json!({"code":"OLD","message":"old","element_id":null,
            "offset":null,"fatal":true});
        let old_decoded: ProcessDiagnostic =
            serde_json::from_value(old_diagnostic.clone()).unwrap();
        assert_eq!(serde_json::to_value(&old_decoded).unwrap(), old_diagnostic);
        let diagnostic = ProcessDiagnostic {
            code: "ESCALATION_IMMEDIATE_PATH_UNSUPPORTED".into(),
            message: "boundary path is invalid".into(),
            element_id: Some("Flow_1".into()),
            offset: Some(42),
            fatal: true,
            boundary_id: Some("Boundary_1".into()),
            flow_id: Some("Flow_1".into()),
            node_id: Some("End_1".into()),
            reason: Some(ProcessEscalationPathReason::TerminalBeforeWait),
        };
        assert_eq!(
            crate::cbor::decode::<ProcessDiagnostic>(&crate::cbor::encode(&diagnostic).unwrap())
                .unwrap(),
            diagnostic
        );
    }

    #[test]
    fn inclusive_gateway_appends_a_typed_variant_without_changing_old_parallel_shape() {
        let parallel = ProcessNodeKind::ParallelGateway;
        assert_eq!(
            serde_json::to_string(&parallel).unwrap(),
            "\"ParallelGateway\""
        );
        assert_eq!(
            crate::cbor::decode::<ProcessNodeKind>(&crate::cbor::encode(&parallel).unwrap())
                .unwrap(),
            parallel
        );
        let inclusive = ProcessNodeKind::InclusiveGateway {
            default_flow_id: Some("Flow_default".into()),
        };
        let bytes = crate::cbor::encode(&inclusive).unwrap();
        assert_eq!(crate::cbor::decode::<ProcessNodeKind>(&bytes).unwrap(), inclusive);
        assert_eq!(serde_json::to_value(&inclusive).unwrap(), serde_json::json!({
            "InclusiveGateway": { "default_flow_id": "Flow_default" }
        }));
        assert!(serde_json::from_value::<ProcessNodeKind>(serde_json::json!({
            "InclusiveGateway": { "default_flow_id": null, "unknown": true }
        })).is_err());
    }

    #[test]
    fn terminate_end_appends_a_unit_variant_without_changing_old_terminal_bytes() {
        let old_end = serde_json::to_vec(&ProcessNodeKind::End).unwrap();
        let old_error = serde_json::to_vec(&ProcessNodeKind::ErrorEnd { error_ref: "Error_1".into() }).unwrap();
        assert_eq!(old_end, br#""End""#);
        assert_eq!(old_error, br#"{"ErrorEnd":{"error_ref":"Error_1"}}"#);
        let terminate = ProcessNodeKind::TerminateEnd;
        assert_eq!(serde_json::to_vec(&terminate).unwrap(), br#""TerminateEnd""#);
        assert_eq!(crate::cbor::decode::<ProcessNodeKind>(&crate::cbor::encode(&terminate).unwrap()).unwrap(), terminate);
    }

    #[test]
    fn script_task_appends_required_fields_without_changing_old_node_bytes() {
        let old_end = serde_json::to_vec(&ProcessNodeKind::End).unwrap();
        let old_terminate = serde_json::to_vec(&ProcessNodeKind::TerminateEnd).unwrap();
        let script = ProcessNodeKind::ScriptTask {
            script: "null".into(), output_mapping: BTreeMap::new(),
        };
        assert_eq!(old_end, br#""End""#);
        assert_eq!(old_terminate, br#""TerminateEnd""#);
        assert_eq!(serde_json::to_vec(&script).unwrap(),
            br#"{"ScriptTask":{"script":"null","output_mapping":{}}}"#);
        let encoded = crate::cbor::encode(&script).unwrap();
        assert_eq!(encoded, b"\xa1\x6aScriptTask\xa2\x66script\x64null\x6eoutput_mapping\xa0");
        assert_eq!(crate::cbor::decode::<ProcessNodeKind>(&encoded).unwrap(), script);
        for invalid in [
            serde_json::json!({"ScriptTask": {"script": "null"}}),
            serde_json::json!({"ScriptTask": {"output_mapping": {}}}),
            serde_json::json!({"ScriptTask": {"script": "null", "output_mapping": {}, "unknown": 1}}),
        ] {
            assert!(serde_json::from_value::<ProcessNodeKind>(invalid).is_err());
        }
    }

    #[test]
    fn message_variants_round_trip_without_rewriting_opaque_payload_or_old_model_bytes() {
        let old = ProcessModel {
            schema_version: 1,
            process_id: "P_1".into(),
            nodes: Vec::new(),
            sequence_flows: Vec::new(),
            variables: BTreeMap::new(),
            diagram: ProcessDiagram::default(),
            timer_timezone: None,
            work_calendar: None,
            calendar_pin: None,
            messages: Vec::new(),
            errors: Vec::new(),
            target_namespace: None,
            escalations: Vec::new(),
            signals: Vec::new(),
            process_name: None, additional_processes: Vec::new(), modeling: None, collaboration: None, data_stores: Vec::new(),
        };
        assert_eq!(serde_json::to_string(&old).unwrap(),
            "{\"schema_version\":1,\"process_id\":\"P_1\",\"nodes\":[],\"sequence_flows\":[],\"variables\":{},\"diagram\":{\"shapes\":[],\"edges\":[]}}");
        let mut model = old;
        model.messages.push(ProcessMessageDeclaration { message_id: "Message_1".into(), name: "order.received".into() });
        model.errors.push(ProcessErrorDeclaration { error_id: "Error_1".into(), name: "Validation".into(), error_code: "BUSINESS.INVALID".into() });
        model.target_namespace = Some("urn:example:orders".into());
        model.nodes.push(ProcessNode { repeat: None, activity_io: None, id: "Start_1".into(), name: "Start".into(), kind: ProcessNodeKind::MessageStart {
            message_ref: "Message_1".into(), output_mapping: BTreeMap::from([("business_key".into(), "outputs.customer_ID".into())]),
        } });
        for kind in [
            ProcessNodeKind::MessageCatch { message_ref: "Message_1".into(), correlation_expression: "vars.case_id".into(), output_mapping: BTreeMap::new() },
            ProcessNodeKind::MessageThrow { message_ref: "Message_1".into(), target: ProcessMessageTargetSpec::Catch {
                definition_id: "7c865aaa-febd-4621-9ae6-35977200a0fd".into(), instance_id_expression: None, subscription_id_expression: None,
            }, correlation_expression: "vars.case_id".into(), payload_expression: "vars.payload".into(), ttl_seconds: 60 },
            ProcessNodeKind::BoundaryMessage { attached_to_id: "Task_1".into(), cancel_activity: false, message_ref: "Message_1".into(), correlation_expression: "vars.case_id".into(), output_mapping: BTreeMap::new() },
            ProcessNodeKind::EventBasedGateway,
            ProcessNodeKind::BoundaryError { attached_to_id: "Task_1".into(), error_ref: Some("Error_1".into()), output_mapping: BTreeMap::new() },
        ] {
            model.nodes.push(ProcessNode { repeat: None, activity_io: None, id: format!("Node_{}", model.nodes.len()), name: String::new(), kind });
        }
        assert_eq!(crate::cbor::decode::<ProcessModel>(&crate::cbor::encode(&model).unwrap()).unwrap(), model);
        let request = ProcessPayload::MessageSendRequest { command_id: "cmd".into(), message_id: "msg".into(),
            target: ProcessMessageTarget::Start { definition_id: "def".into(), process_id: None, start_node_id: None }, message_name: "order.received".into(),
            correlation_key: "key".into(), payload: serde_json::json!({"customer_ID":{"attached_to_id":null}}), ttl_seconds: 60 };
        assert_eq!(crate::cbor::decode::<ProcessPayload>(&crate::cbor::encode(&request).unwrap()).unwrap(), request);
        assert!(serde_json::from_value::<ProcessMessageDeclaration>(serde_json::json!({"message_id":"M","name":"N","unknown":true})).is_err());
        assert!(serde_json::from_value::<ProcessMessageTargetSpec>(serde_json::json!({"Start":{"definition_id":"d","instance_id_expression":"x"}})).is_err());
    }

    #[test]
    fn available_json_null_message_payload_remains_distinct_from_unavailable_payload() {
        let summary = ProcessMessageSummary {
            message_id: "m".into(), sender_user_id: "u".into(), origin: ProcessMessageOrigin::Api,
            target: ProcessMessageTarget::Start { definition_id: "d".into(), process_id: None, start_node_id: None },
            message_name: "order.received".into(), correlation_key: "key".into(), revision: 1,
            status: ProcessMessageStatus::Pending, received_at_ms: 1, expires_at_ms: 2,
            updated_at_ms: 1, delivered_at_ms: None, matched_instance_id: None,
            matched_version: None, matched_subscription_id: None, source_instance_id: None,
            source_node_id: None, last_reason: None, payload_sha256: "sha".into(),
            payload_bytes: 4, payload_available: true, can_resolve: false, can_cancel: true,
            source_scope_id: None,
        };
        let available = ProcessMessageDetail { message: summary.clone(), payload: Some(Value::Null) };
        let bytes = crate::cbor::encode(&available).unwrap();
        assert_eq!(crate::cbor::decode::<ProcessMessageDetail>(&bytes).unwrap(), available);
        assert!(serde_json::to_value(&available).unwrap().get("payload").is_some());
        let unavailable = ProcessMessageDetail { message: ProcessMessageSummary {
            payload_available: false, ..summary
        }, payload: None };
        let bytes = crate::cbor::encode(&unavailable).unwrap();
        assert_eq!(crate::cbor::decode::<ProcessMessageDetail>(&bytes).unwrap(), unavailable);
        assert!(serde_json::to_value(&unavailable).unwrap().get("payload").is_none());
    }

    #[test]
    fn empty_instance_message_collections_remain_required_arrays_on_the_wire() {
        let instance: ProcessInstance = serde_json::from_value(serde_json::json!({
            "instance_id": "i1", "definition_id": "d1", "definition_name": "Approval",
            "initiator_user_id": "u1", "version": 1, "revision": 1,
            "status": "Running", "variables": {}, "active_node_ids": [],
            "user_tasks": [], "incidents": [], "created_at_ms": 1,
            "updated_at_ms": 1, "can_cancel": true, "can_retry": false,
            "can_send_message": true, "calls": [],
            "repetition_groups": [], "repetition_occurrences": [],
            "selected_repetition_occurrence": null, "process_id":"P_1", "start_node_id":"Start_1"
        })).unwrap();
        let body = crate::message_body::MessageBody::ProcessBody(
            ProcessPayload::InstanceGetResponse { instance },
        );
        let decoded: crate::message_body::MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&body).unwrap()).unwrap();
        let json = serde_json::to_value(decoded).unwrap();
        let instance = &json["ProcessBody"]["InstanceGetResponse"]["instance"];
        for field in ["subscriptions", "event_races", "outgoing_messages", "message_names", "scopes", "calls", "repetition_groups", "repetition_occurrences"] {
            assert_eq!(instance[field], serde_json::json!([]), "{field} must be an array");
        }
        assert!(instance["selected_repetition_occurrence"].is_null());
        assert_eq!(instance["can_send_message"], true);
    }

    #[test]
    fn timer_rules_round_trip_and_timerless_model_omits_new_fields() {
        let timerless = ProcessModel {
            schema_version: 1,
            process_id: "P_1".into(),
            nodes: Vec::new(),
            sequence_flows: Vec::new(),
            variables: BTreeMap::new(),
            diagram: ProcessDiagram::default(),
            timer_timezone: None,
            work_calendar: None,
            calendar_pin: None,
            messages: Vec::new(),
            errors: Vec::new(),
            target_namespace: None,
            escalations: Vec::new(),
            signals: Vec::new(),
            process_name: None, additional_processes: Vec::new(), modeling: None, collaboration: None, data_stores: Vec::new(),
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
            model.nodes.push(ProcessNode { repeat: None, activity_io: None, id: "Start_1".into(), name: "Start".into(), kind: node_kind });
            let bytes = crate::cbor::encode(&model).unwrap();
            let decoded: ProcessModel = crate::cbor::decode(&bytes).unwrap();
            assert_eq!(decoded, model);
        }
        assert!(serde_json::from_value::<ProcessTimerSpec>(serde_json::json!({"Cycle":{"seconds":300,"total_firings":3,"unknown":true}})).is_err());
        assert!(serde_json::from_value::<ProcessModel>(serde_json::json!({"schema_version":1,"process_id":"P_1","nodes":[],"sequence_flows":[],"variables":{},"diagram":{"shapes":[],"edges":[]},"timer_timezone":null,"unknown":true})).is_err());
    }

    #[test]
    fn working_calendar_and_pin_round_trip_without_rewriting_business_keys() {
        let raw = serde_json::json!({
            "schema_version":1,"process_id":"P_1","nodes":[{"id":"Start_1","name":"Start",
                "kind":{"TimerStart":{"timer":{"WorkingDuration":{"seconds":3600}}}}}],
            "sequence_flows":[],"variables":{"business_key":{"inner_value":"Łódź"}},
            "diagram":{"shapes":[],"edges":[]},"timer_timezone":"Europe/Warsaw",
            "work_calendar":{"name":"Office","weekly_windows":[{"weekday":1,"start_minute":540,"end_minute":1020}],
                "manual_days_off":[],"holiday_policy":"PolandStatutory"},
            "calendar_pin":{"calendar":{"name":"Office","weekly_windows":[{"weekday":1,"start_minute":540,"end_minute":1020}],
                    "manual_days_off":[],"holiday_policy":"PolandStatutory"},
                "legal_release":{"release_id":"PL-statutory-2026-10-02","as_of_date":"2026-10-02",
                    "valid_from":"2024-01-01","valid_until":"2041-01-01","audit_manifest_sha256":"a",
                    "sources":[],"rules":[{"rule_id":"sunday","source_id":"DU/2024/1965",
                        "effective_from":"2024-01-01","kind":{"Weekday":{"weekday":7}}}]},
                "timezone_data":{"iana_name":"Europe/Warsaw","release_id":"2026e","horizon_start_ms":1,
                    "horizon_end_ms":2,"initial_offset_seconds":3600,"transitions":[],
                    "source_url":"https://example.invalid","source_sha256":"b","dataset_sha256":"c"},"sha256":"d"}
        });
        let model: ProcessModel = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(&model).unwrap(), raw);
        assert_eq!(crate::cbor::decode::<ProcessModel>(&crate::cbor::encode(&model).unwrap()).unwrap(), model);
        assert_eq!(model.variables["business_key"]["inner_value"], "Łódź");
        let mut invalid = raw;
        invalid["work_calendar"]["weekly_windows"][0]["surprise"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ProcessModel>(invalid).is_err());
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
                nodes: vec![ProcessNode { repeat: None, activity_io: None,
                    id: "Start_1".into(),
                    name: "Start".into(),
                    kind: ProcessNodeKind::Start,
                }],
                sequence_flows: Vec::new(),
                variables: BTreeMap::new(),
                diagram: ProcessDiagram::default(),
                timer_timezone: None,
                work_calendar: None,
                calendar_pin: None,
                messages: Vec::new(),
                errors: Vec::new(),
                target_namespace: None,
                escalations: Vec::new(),
                signals: Vec::new(),
            process_name: None, additional_processes: Vec::new(), modeling: None, collaboration: None, data_stores: Vec::new(),
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
                    calendar_pin_state: None,
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
            token_id: None,
            scope_id: "i1".into(),
            instructions: None,
            activity_inputs: Vec::new(),
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
        assert!(detail["UserTaskGetResponse"]["task"].get("instructions").is_none());
    }

    #[test]
    fn user_task_detail_round_trips_sixteen_near_limit_activity_inputs() {
        let value = Value::String("x".repeat(256 * 1024 - 64));
        let task = ProcessUserTask {
            user_task_id: "task-large".into(),
            node_id: "Review_1".into(),
            name: "Review".into(),
            assignee_user_id: "user-1".into(),
            kind: ProcessUserTaskKind::Work,
            status: ProcessUserTaskStatus::Open,
            outputs: Value::Null,
            revision: 1,
            can_complete: true,
            token_id: Some("waiting-1".into()),
            scope_id: "i1".into(),
            instructions: None,
            activity_inputs: (0..16)
                .map(|position| ProcessUserTaskInput {
                    position,
                    declaration_id: format!("input_{position}"),
                    name: Some(format!("Input {position}")),
                    value: ProcessUserTaskInputValue::Present(value.clone()),
                })
                .collect(),
        };
        let payload = ProcessPayload::UserTaskGetResponse { task: task.clone() };
        let encoded = crate::cbor::encode(&payload).unwrap();
        assert!(encoded.len() > 4 * 1024 * 1024);
        let decoded: ProcessPayload = crate::cbor::decode(&encoded).unwrap();
        assert_eq!(decoded, payload);
        let summary = ProcessUserTaskSummary::from(&task);
        assert!(serde_json::to_value(summary).unwrap().get("activity_inputs").is_none());
    }

    #[test]
    fn manual_acknowledgment_has_distinct_wire_fields_and_pinned_instructions() {
        let kind = ProcessNodeKind::ManualTask {
            assignee_user_id: None,
            instructions: "Inspect the external register.\nRecord the result there.".into(),
        };
        let bytes = crate::cbor::encode(&kind).unwrap();
        assert_eq!(crate::cbor::decode::<ProcessNodeKind>(&bytes).unwrap(), kind);
        assert_eq!(serde_json::to_value(&kind).unwrap(), serde_json::json!({
            "ManualTask": {"assignee_user_id":null,
                "instructions":"Inspect the external register.\nRecord the result there."}
        }));
        assert!(serde_json::from_value::<ProcessNodeKind>(serde_json::json!({
            "ManualTask": {"instructions":"Inspect"}
        })).is_err());
        let request = ProcessPayload::ManualTaskAcknowledgeRequest {
            command_id: "command-1".into(), instance_id: "instance-1".into(),
            user_task_id: "task-1".into(), expected_revision: 4,
        };
        let encoded = crate::cbor::encode(&request).unwrap();
        assert_eq!(crate::cbor::decode::<ProcessPayload>(&encoded).unwrap(), request);
        let body = serde_json::to_value(request).unwrap();
        assert_eq!(body["ManualTaskAcknowledgeRequest"], serde_json::json!({
            "command_id":"command-1","instance_id":"instance-1",
            "user_task_id":"task-1","expected_revision":4
        }));
        assert!(body["ManualTaskAcknowledgeRequest"].get("outputs").is_none());
        assert!(body["ManualTaskAcknowledgeRequest"].get("approved").is_none());
    }

    #[test]
    fn send_and_receive_tasks_append_distinct_wire_variants() {
        let target = ProcessMessageTargetSpec::Catch {
            definition_id: "definition-1".into(), instance_id_expression: Some("vars.instance_id".into()),
            subscription_id_expression: None,
        };
        let send = ProcessNodeKind::SendTask { message_ref: "Message_1".into(), target,
            correlation_expression: "vars.case_key".into(), payload_expression: "vars.payload".into(),
            ttl_seconds: 3600 };
        let receive = ProcessNodeKind::ReceiveTask { message_ref: "Message_1".into(),
            correlation_expression: "vars.case_key".into(),
            output_mapping: BTreeMap::from([("received".into(), "outputs".into())]) };
        for kind in [&send, &receive] {
            let bytes = crate::cbor::encode(kind).unwrap();
            assert_eq!(crate::cbor::decode::<ProcessNodeKind>(&bytes).unwrap(), *kind);
        }
        assert!(serde_json::to_value(&send).unwrap().get("MessageThrow").is_none());
        assert!(serde_json::to_value(&receive).unwrap().get("MessageCatch").is_none());
        assert!(serde_json::from_value::<ProcessNodeKind>(serde_json::json!({
            "ReceiveTask":{"message_ref":"Message_1","correlation_expression":"vars.case_key"}
        })).is_err());
        let subscription = ProcessSubscriptionKind::ReceiveTask;
        let bytes = crate::cbor::encode(&subscription).unwrap();
        assert_eq!(crate::cbor::decode::<ProcessSubscriptionKind>(&bytes).unwrap(), subscription);
        assert_eq!(serde_json::to_value(subscription).unwrap(), "ReceiveTask");
    }

    #[test]
    fn signal_declarations_and_events_append_without_changing_older_wire_shapes() {
        let declaration = ProcessSignalDeclaration { signal_id: "Signal_1".into(),
            namespace_uri: "urn:orders".into(), name: "Order changed".into() };
        let throw = ProcessNodeKind::SignalThrow { signal_ref: declaration.signal_id.clone(),
            payload_expression: "vars.payload".into(), ttl_seconds: 3600 };
        let catch = ProcessNodeKind::SignalCatch { signal_ref: declaration.signal_id.clone(),
            output_mapping: BTreeMap::from([("received".into(), "outputs".into())]) };
        for kind in [&throw, &catch] {
            let bytes = crate::cbor::encode(kind).unwrap();
            assert_eq!(crate::cbor::decode::<ProcessNodeKind>(&bytes).unwrap(), *kind);
        }
        assert_eq!(serde_json::to_value(&throw).unwrap(), serde_json::json!({
            "SignalThrow":{"signal_ref":"Signal_1","payload_expression":"vars.payload","ttl_seconds":3600}
        }));
        assert!(serde_json::from_value::<ProcessNodeKind>(serde_json::json!({
            "SignalCatch":{"signal_ref":"Signal_1"}
        })).is_err());
        let bytes = crate::cbor::encode(&ProcessSubscriptionKind::SignalCatch).unwrap();
        assert_eq!(crate::cbor::decode::<ProcessSubscriptionKind>(&bytes).unwrap(), ProcessSubscriptionKind::SignalCatch);
        assert_eq!(serde_json::to_value(ProcessSubscriptionKind::SignalCatch).unwrap(), "SignalCatch");
        assert_eq!(serde_json::to_value(declaration).unwrap(), serde_json::json!({
            "signal_id":"Signal_1","namespace_uri":"urn:orders","name":"Order changed"
        }));
    }

    #[test]
    fn boundary_identity_round_trips_without_changing_older_timer_and_task_json() {
        assert_eq!(
            serde_json::to_string(&ProcessNodeKind::TimerCatch {
                timer: ProcessTimerSpec::Duration { seconds: 90 },
            }).unwrap(),
            "{\"TimerCatch\":{\"timer\":{\"Duration\":{\"seconds\":90}}}}"
        );
        let boundary = ProcessNodeKind::BoundaryTimer {
            attached_to_id: "Review_1".into(),
            cancel_activity: false,
            timer: ProcessTimerSpec::Duration { seconds: 90 },
        };
        let encoded = crate::cbor::encode(&boundary).unwrap();
        assert_eq!(crate::cbor::decode::<ProcessNodeKind>(&encoded).unwrap(), boundary);
        assert_eq!(serde_json::to_value(&boundary).unwrap(), serde_json::json!({
            "BoundaryTimer":{"attached_to_id":"Review_1","cancel_activity":false,
                "timer":{"Duration":{"seconds":90}}}
        }));
        assert!(serde_json::from_value::<ProcessNodeKind>(serde_json::json!({
            "BoundaryTimer": {"attached_to_id":"Review_1","cancel_activity":false,
                "timer":{"Duration":{"seconds":90}},"unsupported":true}
        })).is_err());
        let timer = ProcessTimerSummary {
            timer_id: "timer-1".into(), node_id: "Boundary_1".into(),
            node_name: "Timeout".into(), kind: ProcessTimerKind::Boundary,
            status: ProcessTimerStatus::Pending, due_at_ms: Some(1_000),
            timezone: "UTC".into(), occurrence: 1, total_firings: None,
            last_reason: None, attached_to_id: Some("Review_1".into()),
            working_time: None,
            scope_id: None,
        };
        assert_eq!(crate::cbor::decode::<ProcessTimerSummary>(&crate::cbor::encode(&timer).unwrap()).unwrap(), timer);
        assert_eq!(serde_json::to_value(&timer).unwrap()["attached_to_id"], "Review_1");
        let mut earlier = timer;
        earlier.kind = ProcessTimerKind::Catch;
        earlier.attached_to_id = None;
        assert!(serde_json::to_value(&earlier).unwrap().get("attached_to_id").is_none());
        let older_task = ProcessUserTask {
            user_task_id: "task-1".into(), node_id: "Review_1".into(),
            name: "Review".into(), assignee_user_id: "user-1".into(),
            kind: ProcessUserTaskKind::Work, status: ProcessUserTaskStatus::Completed,
            outputs: Value::Null, revision: 2, can_complete: false, token_id: None,
            scope_id: "i1".into(),
            instructions: None,
            activity_inputs: Vec::new(),
        };
        assert!(serde_json::to_value(&older_task).unwrap().get("token_id").is_none());
        assert!(serde_json::to_value(&older_task).unwrap().get("instructions").is_none());
        let mut active = older_task;
        active.status = ProcessUserTaskStatus::Open;
        active.token_id = Some("waiting-1".into());
        assert_eq!(serde_json::to_value(&active).unwrap()["token_id"], "waiting-1");
        active.kind = ProcessUserTaskKind::Manual;
        active.instructions = Some("Read the external report".into());
        assert_eq!(serde_json::to_value(&active).unwrap()["instructions"], "Read the external report");
        assert_eq!(active.outputs, Value::Null);
    }

    #[test]
    fn embedded_scope_body_and_detail_round_trip_without_rewriting_opaque_variables() {
        let body = ProcessSubProcess {
            nodes: vec![
                ProcessNode { repeat: None, activity_io: None, id: "Child_Start".into(), name: "Enter".into(), kind: ProcessNodeKind::Start },
                ProcessNode { repeat: None, activity_io: None, id: "Child_End".into(), name: "Leave".into(), kind: ProcessNodeKind::End },
            ],
            sequence_flows: vec![ProcessSequenceFlow { id: "Child_Flow".into(),
                source_id: "Child_Start".into(), target_id: "Child_End".into(), condition: None, call_start_node_id: None }],
            variables: BTreeMap::from([("customer_ID".into(), serde_json::json!({"original_key": 7}))]),
            diagram: ProcessDiagram::default(), modeling: None,
        };
        let kind = ProcessNodeKind::SubProcess { body, input_mapping: BTreeMap::from([
            ("local_ID".into(), "vars.customer_ID".into()),
        ]), output_mapping: BTreeMap::new() };
        assert_eq!(crate::cbor::decode::<ProcessNodeKind>(&crate::cbor::encode(&kind).unwrap()).unwrap(), kind);
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["SubProcess"]["body"]["variables"]["customer_ID"]["original_key"], 7);
        assert!(serde_json::from_value::<ProcessNodeKind>(serde_json::json!({
            "SubProcess": {"body": {"nodes": [], "sequence_flows": [], "variables": {},
                "diagram": {"shapes": [], "edges": []}, "timer_timezone": "UTC"},
                "input_mapping": {}, "output_mapping": {}}
        })).is_err());
        let scope = ProcessScopeSummary { scope_id: "child-1".into(), parent_scope_id: Some("instance-1".into()),
            subprocess_node_id: Some("Child_1".into()), subprocess_node_name: Some("Review".into()),
            parent_token_id: Some("wait-1".into()), revision: 2, status: ProcessInstanceStatus::Running,
            depth: 1, created_at_ms: 100, updated_at_ms: 200, terminal_error: None };
        let payload = ProcessPayload::ScopeGetResponse { scope: scope.clone(),
            variables: serde_json::json!({"customer_ID":{"original_key":7}}),
            active_node_ids: vec!["Child_Start".into()] };
        assert_eq!(crate::cbor::decode::<ProcessPayload>(&crate::cbor::encode(&payload).unwrap()).unwrap(), payload);
        assert_eq!(serde_json::to_value(scope).unwrap()["scope_id"], "child-1");
    }

    #[test]
    fn call_activity_and_error_end_keep_opaque_mappings_and_exact_target_identity() {
        let reference = ProcessCallableReference {
            namespace_uri: "urn:example:approval:v1".into(),
            process_id: "Approval_1".into(),
        };
        let call = ProcessNodeKind::CallActivity(ProcessCallActivity {
            target: ProcessCallTarget::PublishedBody {
                definition_id: "da53c1fd-c235-40ec-bde1-44cf57bb620d".into(),
                version: 7,
                called_element: reference.clone(),
            },
            input_mapping: BTreeMap::from([("customer_ID".into(), "vars.customer_ID".into())]),
            output_mapping: BTreeMap::from([("approved_value".into(), "outputs.business_key".into())]),
        });
        assert_eq!(serde_json::to_string(&call).unwrap(),
            r#"{"CallActivity":{"called_definition_id":"da53c1fd-c235-40ec-bde1-44cf57bb620d","called_version":7,"called_element":{"namespace_uri":"urn:example:approval:v1","process_id":"Approval_1"},"input_mapping":{"customer_ID":"vars.customer_ID"},"output_mapping":{"approved_value":"outputs.business_key"}}}"#);
        assert_eq!(crate::cbor::decode::<ProcessNodeKind>(&crate::cbor::encode(&call).unwrap()).unwrap(), call);
        let json = serde_json::to_value(&call).unwrap();
        assert_eq!(json["CallActivity"]["called_element"]["namespace_uri"], "urn:example:approval:v1");
        assert_eq!(json["CallActivity"]["input_mapping"]["customer_ID"], "vars.customer_ID");
        assert_eq!(json["CallActivity"]["output_mapping"]["approved_value"], "outputs.business_key");
        assert!(serde_json::from_value::<ProcessNodeKind>(serde_json::json!({
            "CallActivity": {"called_definition_id": "d", "called_version": 1,
                "called_element": {"namespace_uri": "urn:test", "process_id": "P", "unknown": true},
                "input_mapping": {}, "output_mapping": {}}
        })).is_err());
        for invalid in [
            r#"{"CallActivity":{"local_body":{"namespace_uri":"urn:test","process_id":"P"},"called_definition_id":"d","input_mapping":{},"output_mapping":{}}}"#,
            r#"{"CallActivity":{"local_body":{"namespace_uri":"urn:test","process_id":"P"},"local_body":{"namespace_uri":"urn:test","process_id":"Q"},"input_mapping":{},"output_mapping":{}}}"#,
            r#"{"CallActivity":{"called_definition_id":"d","called_version":7,"called_element":{"namespace_uri":"urn:test","process_id":"P"},"input_mapping":{}}}"#,
            r#"{"CallActivity":{"called_definition_id":"d","called_version":0,"called_element":{"namespace_uri":"urn:test","process_id":"P"},"input_mapping":{},"output_mapping":{}}}"#,
            r#"{"CallActivity":{"local_body":{"namespace_uri":"urn:test","process_id":"P"},"input_mapping":{},"output_mapping":{},"unknown":1}}"#,
        ] {
            assert!(serde_json::from_str::<ProcessNodeKind>(invalid).is_err());
        }
        let local = ProcessNodeKind::CallActivity(ProcessCallActivity {
            target: ProcessCallTarget::LocalBody { called_element: reference.clone() },
            input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
        });
        assert_eq!(serde_json::to_string(&local).unwrap(),
            r#"{"CallActivity":{"local_body":{"namespace_uri":"urn:example:approval:v1","process_id":"Approval_1"},"input_mapping":{},"output_mapping":{}}}"#);
        assert_eq!(crate::cbor::decode::<ProcessNodeKind>(&crate::cbor::encode(&local).unwrap()).unwrap(), local);
        let error_end = ProcessNodeKind::ErrorEnd { error_ref: "Error_1".into() };
        assert_eq!(crate::cbor::decode::<ProcessNodeKind>(&crate::cbor::encode(&error_end).unwrap()).unwrap(), error_end);
        assert_eq!(reference.process_id, "Approval_1");
    }

    #[test]
    fn start_catalog_keeps_body_identity_and_absent_process_name_distinct() {
        let entry = ProcessStartCatalogEntry {
            process_id: "Approval_1".into(),
            process_name: None,
            start_node_id: "Start_1".into(),
            start_node_name: "Begin".into(),
            version: 7,
            trigger: ProcessStartTrigger::MessageStart {
                message_ref: "Message_1".into(),
                message_name: "approval.requested".into(),
                can_send: true,
            },
        };
        assert_eq!(serde_json::to_string(&entry).unwrap(),
            r#"{"process_id":"Approval_1","process_name":null,"start_node_id":"Start_1","start_node_name":"Begin","version":7,"trigger":{"MessageStart":{"message_ref":"Message_1","message_name":"approval.requested","can_send":true}}}"#);
        assert_eq!(crate::cbor::decode::<ProcessStartCatalogEntry>(
            &crate::cbor::encode(&entry).unwrap()).unwrap(), entry);
        let mut authored_empty = entry;
        authored_empty.process_name = Some(String::new());
        assert_eq!(serde_json::to_value(&authored_empty).unwrap()["process_name"], "");
    }

    #[test]
    fn call_response_arrays_and_terminal_error_are_typed_without_link_disclosure() {
        let model = ProcessModel {
            schema_version: 1,
            process_id: "P_1".into(),
            nodes: Vec::new(),
            sequence_flows: Vec::new(),
            variables: BTreeMap::new(),
            diagram: ProcessDiagram::default(),
            timer_timezone: None,
            work_calendar: None,
            calendar_pin: None,
            messages: Vec::new(),
            errors: Vec::new(),
            target_namespace: None,
            escalations: Vec::new(),
            signals: Vec::new(),
            process_name: None, additional_processes: Vec::new(), modeling: None, collaboration: None, data_stores: Vec::new(),
        };
        let version = ProcessVersion {
            definition_id: "definition".into(),
            version: 1,
            model,
            published_at_ms: 1,
            published_by: "owner".into(),
            model_sha256: "sha".into(),
            service_flows: Vec::new(),
            call_activities: Vec::new(),
        };
        let json = serde_json::to_value(&version).unwrap();
        assert_eq!(json["call_activities"], serde_json::json!([]));
        assert_eq!(crate::cbor::decode::<ProcessVersion>(&crate::cbor::encode(&version).unwrap()).unwrap(), version);

        let page = ProcessPageInfo { offset: 0, total: 0, next_offset: None, has_more: false };
        let pages = ProcessInstancePageInfo {
            user_tasks: page.clone(), incidents: page.clone(), timers: page.clone(),
            subscriptions: page.clone(), event_races: page.clone(), outgoing_messages: page.clone(),
            scopes: page.clone(), calls: page.clone(), repetition_groups: page.clone(),
            repetition_occurrences: page,
        };
        let terminal_error = ProcessTerminalError {
            error_ref: "Error_1".into(), error_code: "BUSINESS.INVALID".into(),
            source_event_id: "event-1".into(), source_node_id: "ErrorEnd_1".into(),
            source_scope_id: "instance".into(),
        };
        let instance = ProcessInstance {
            instance_id: "instance".into(), definition_id: "definition".into(),
            definition_name: "Approval".into(), initiator_user_id: "owner".into(),
            version: 1, revision: 2, status: ProcessInstanceStatus::Error,
            variables: serde_json::json!({"business_key": {"inner_value": 7}}),
            active_node_ids: Vec::new(), user_tasks: Vec::new(), incidents: Vec::new(),
            created_at_ms: 1, updated_at_ms: 2, can_cancel: false, can_retry: false,
            timers: Vec::new(), subscriptions: Vec::new(), event_races: Vec::new(),
            outgoing_messages: Vec::new(), message_names: Vec::new(), can_send_message: None,
            pages: Some(pages), selected_user_task: None, selected_incident: None,
            scopes: Vec::new(), calls: vec![ProcessCallSummary::Incoming { parent: None }],
            terminal_error: Some(terminal_error),
            repetition_groups: Vec::new(), repetition_occurrences: Vec::new(),
            selected_repetition_occurrence: None, process_id: "P_1".into(), start_node_id: "Start_1".into(),
        };
        let encoded = crate::cbor::encode(&instance).unwrap();
        assert_eq!(crate::cbor::decode::<ProcessInstance>(&encoded).unwrap(), instance);
        let json = serde_json::to_value(&instance).unwrap();
        assert_eq!(json["pages"]["calls"]["total"], 0);
        assert_eq!(json["pages"]["repetition_groups"]["total"], 0);
        assert_eq!(json["repetition_groups"], serde_json::json!([]));
        assert_eq!(json["calls"][0], serde_json::json!({"Incoming":{"parent":null}}));
        assert!(json["calls"][0].get("call_node_id").is_none());
        assert_eq!(json["terminal_error"]["source_event_id"], "event-1");
        let empty = ProcessInstance { calls: Vec::new(), terminal_error: None, ..instance };
        let json = serde_json::to_value(empty).unwrap();
        assert_eq!(json["calls"], serde_json::json!([]));
        assert!(json.get("terminal_error").is_none());
    }
}
