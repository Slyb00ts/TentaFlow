use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ProcessCallActivity, ProcessCallTarget, ProcessCallableReference,
    ProcessBodyModeling, ProcessDataStore, ProcessDataStoreReference, ProcessModel,
    ProcessNodeKind, ProcessStartCatalogEntry, ProcessStartTrigger,
    ProcessTimerKind, ProcessTimerSpec, ProcessTimerStatus, ProcessTimerSummary,
    ProcessWorkingTimeSummary, HolidayPolicy,
};

fn callable_reference() -> ProcessCallableReference {
    ProcessCallableReference {
        namespace_uri: "urn:example:approval".into(),
        process_id: "Approval_1".into(),
    }
}

fn published_call() -> ProcessNodeKind {
    ProcessNodeKind::CallActivity(ProcessCallActivity {
        target: ProcessCallTarget::PublishedBody {
            definition_id: "definition-1".into(),
            version: 7,
            called_element: callable_reference(),
        },
        input_mapping: BTreeMap::from([("customer_ID".into(), "vars.customer_ID".into())]),
        output_mapping: BTreeMap::from([("approved_value".into(), "outputs.business_key".into())]),
    })
}

fn catalog_entry(process_name: Option<&str>, trigger: ProcessStartTrigger) -> ProcessStartCatalogEntry {
    ProcessStartCatalogEntry {
        process_id: "Approval_1".into(),
        process_name: process_name.map(str::to_owned),
        start_node_id: match &trigger {
            ProcessStartTrigger::MessageStart { .. } => "Start_Message",
            ProcessStartTrigger::TimerStart { .. } => "Start_Timer",
            ProcessStartTrigger::Start { .. } => "Start_Manual",
        }
        .into(),
        start_node_name: "Start node".into(),
        version: 7,
        trigger,
    }
}

fn timer_summary() -> ProcessTimerSummary {
    ProcessTimerSummary {
        timer_id: "timer-1".into(),
        node_id: "Start_Timer".into(),
        node_name: "Timer start".into(),
        kind: ProcessTimerKind::Start,
        status: ProcessTimerStatus::Pending,
        due_at_ms: Some(1_791_234_567_000),
        timezone: "Europe/Warsaw".into(),
        occurrence: 1,
        total_firings: None,
        last_reason: None,
        attached_to_id: None,
        working_time: None,
        scope_id: None,
    }
}

#[test]
fn published_call_keeps_the_original_five_field_json_shape_and_order() {
    let call = published_call();
    let expected = include_str!("fixtures/published-call-canonical.json").trim_end();

    assert_eq!(serde_json::to_string(&call).unwrap(), expected);
    assert_eq!(serde_json::from_str::<ProcessNodeKind>(expected).unwrap(), call);
}

#[test]
fn local_call_keeps_exact_qname_and_rejects_published_target_mixing() {
    let call = ProcessNodeKind::CallActivity(ProcessCallActivity {
        target: ProcessCallTarget::LocalBody {
            called_element: callable_reference(),
        },
        input_mapping: BTreeMap::new(),
        output_mapping: BTreeMap::new(),
    });
    let expected = include_str!("fixtures/local-call-canonical.json").trim_end();
    assert_eq!(serde_json::to_string(&call).unwrap(), expected);

    let decoded: ProcessNodeKind = serde_json::from_str(expected).unwrap();
    let ProcessNodeKind::CallActivity(ProcessCallActivity {
        target: ProcessCallTarget::LocalBody { called_element },
        ..
    }) = decoded
    else {
        panic!("local call target changed variant");
    };
    assert_eq!(called_element.namespace_uri, "urn:example:approval");
    assert_eq!(called_element.process_id, "Approval_1");

    for malformed in [
        r#"{"CallActivity":{"local_body":{"namespace_uri":"urn:test","process_id":"P"},"called_definition_id":"d","input_mapping":{},"output_mapping":{}}}"#,
        r#"{"CallActivity":{"local_body":{"namespace_uri":"urn:test","process_id":"P"},"called_version":7,"input_mapping":{},"output_mapping":{}}}"#,
        r#"{"CallActivity":{"local_body":{"namespace_uri":"urn:test","process_id":"P"},"input_mapping":{}}}"#,
        r#"{"CallActivity":{"local_body":{"namespace_uri":"urn:test","process_id":"P"},"input_mapping":{},"output_mapping":{},"unknown":true}}"#,
        r#"{"CallActivity":{"local_body":{"namespace_uri":"urn:test","namespace_uri":"urn:other","process_id":"P"},"input_mapping":{},"output_mapping":{}}}"#,
        r#"{"CallActivity":{"local_body":{"namespace_uri":"urn:test","process_id":"P"},"local_body":{"namespace_uri":"urn:other","process_id":"Q"},"input_mapping":{},"output_mapping":{}}}"#,
    ] {
        assert!(serde_json::from_str::<ProcessNodeKind>(malformed).is_err(), "accepted {malformed}");
    }
}

#[test]
fn one_body_model_omits_appended_tails_and_cbor_reencode_is_byte_stable() {
    let one_body_model_json = r#"{"schema_version":1,"process_id":"Parent_1","nodes":[{"id":"Call_1","name":"Approval","kind":{"CallActivity":{"called_definition_id":"definition-1","called_version":7,"called_element":{"namespace_uri":"urn:example:approval","process_id":"Approval_1"},"input_mapping":{"customer_ID":"vars.customer_ID"},"output_mapping":{"approved_value":"outputs.business_key"}}}}],"sequence_flows":[],"variables":{"business_key":{"customer_ID":7}},"diagram":{"shapes":[],"edges":[]}}"#;
    let model: ProcessModel = serde_json::from_str(one_body_model_json).unwrap();
    let serialized = serde_json::to_string(&model).unwrap();
    let serialized_value: Value = serde_json::from_str(&serialized).unwrap();

    for appended_field in ["process_name", "additional_processes", "modeling", "collaboration", "data_stores"] {
        assert!(serialized_value.get(appended_field).is_none(), "unexpected {appended_field}");
    }
    assert_eq!(
        serialized_value["nodes"][0]["kind"]["CallActivity"],
        json!({
            "called_definition_id":"definition-1",
            "called_version":7,
            "called_element":{"namespace_uri":"urn:example:approval","process_id":"Approval_1"},
            "input_mapping":{"customer_ID":"vars.customer_ID"},
            "output_mapping":{"approved_value":"outputs.business_key"}
        })
    );

    let encoded = tentaflow_protocol::cbor::encode(&model).unwrap();
    let decoded: ProcessModel = tentaflow_protocol::cbor::decode(&encoded).unwrap();
    assert_eq!(decoded, model);
    assert_eq!(tentaflow_protocol::cbor::encode(&decoded).unwrap(), encoded);
}

#[test]
fn data_store_declaration_and_body_reference_preserve_optional_metadata() {
    let mut model: ProcessModel = serde_json::from_str(include_str!("fixtures/model.json")).unwrap();
    model.data_stores.push(ProcessDataStore {
        id: "Store_1".into(),
        name: Some(String::new()),
        capacity: Some(12),
        is_unlimited: Some(false),
    });
    model.modeling = Some(ProcessBodyModeling {
        data_store_references: vec![ProcessDataStoreReference {
            id: "StoreRef_1".into(),
            name: None,
            data_store_ref: "Store_1".into(),
        }],
        ..ProcessBodyModeling::default()
    });
    let json = serde_json::to_string(&model).unwrap();
    assert_eq!(serde_json::from_str::<ProcessModel>(&json).unwrap(), model);
    assert!(json.contains(r#""data_stores":[{"id":"Store_1","name":"","capacity":12,"is_unlimited":false}]"#));
    assert!(json.contains(r#""data_store_references":[{"id":"StoreRef_1","data_store_ref":"Store_1"}]"#));
    let cbor = tentaflow_protocol::cbor::encode(&model).unwrap();
    assert_eq!(tentaflow_protocol::cbor::decode::<ProcessModel>(&cbor).unwrap(), model);
    assert!(serde_json::from_value::<ProcessDataStore>(json!({"id":"Store_1","unknown":1})).is_err());
    assert!(serde_json::from_value::<ProcessDataStoreReference>(json!({"id":"StoreRef_1","data_store_ref":"Store_1","unknown":1})).is_err());
}

#[test]
fn catalog_process_name_distinguishes_null_from_authored_empty_string() {
    let fixture = include_str!("fixtures/start-catalog-message.json").trim_end();
    let entry: ProcessStartCatalogEntry = serde_json::from_str(fixture).unwrap();
    assert_eq!(serde_json::to_string(&entry).unwrap(), fixture);
    assert_eq!(entry.process_name, None);
    assert_eq!(tentaflow_protocol::cbor::decode::<ProcessStartCatalogEntry>(
        &tentaflow_protocol::cbor::encode(&entry).unwrap(),
    ).unwrap(), entry);

    let empty = catalog_entry(
        Some(""),
        ProcessStartTrigger::Start { can_start: true },
    );
    assert_eq!(serde_json::to_value(&empty).unwrap()["process_name"], "");

    let mut missing_name: Value = serde_json::from_str(fixture).unwrap();
    missing_name.as_object_mut().unwrap().remove("process_name");
    assert!(serde_json::from_value::<ProcessStartCatalogEntry>(missing_name.clone()).is_err());
    assert!(tentaflow_protocol::cbor::decode::<ProcessStartCatalogEntry>(
        &tentaflow_protocol::cbor::encode(&missing_name).unwrap(),
    ).is_err());
}

#[test]
fn catalog_typed_message_and_timer_start_fields_roundtrip_and_require_nullable_keys() {
    let message = catalog_entry(
        None,
        ProcessStartTrigger::MessageStart {
            message_ref: "Message_1".into(),
            message_name: "approval.requested".into(),
            can_send: true,
        },
    );
    let timer = ProcessStartCatalogEntry {
        process_id: "Approval_1".into(),
        process_name: Some(String::new()),
        start_node_id: "Start_Timer".into(),
        start_node_name: "Timer start".into(),
        version: 7,
        trigger: ProcessStartTrigger::TimerStart {
            timer: ProcessTimerSpec::Duration { seconds: 90 },
            timezone: "Europe/Warsaw".into(),
            working_time: None,
            persisted_timer: Some(timer_summary()),
        },
    };

    let timer_json = serde_json::to_string(&timer).unwrap();
    let fixture = include_str!("fixtures/start-catalog-timer.json").trim_end();
    let actual: Value = serde_json::from_str(&timer_json).unwrap();
    assert_eq!(timer_json, fixture);
    assert_eq!(actual["trigger"]["TimerStart"]["timer"], json!({"Duration":{"seconds":90}}));
    assert_eq!(actual["trigger"]["TimerStart"]["timezone"], "Europe/Warsaw");

    let working_timer = ProcessStartCatalogEntry {
        process_id: "Approval_1".into(),
        process_name: Some(String::new()),
        start_node_id: "Start_Timer".into(),
        start_node_name: "Timer start".into(),
        version: 7,
        trigger: ProcessStartTrigger::TimerStart {
            timer: ProcessTimerSpec::Duration { seconds: 90 },
            timezone: "Europe/Warsaw".into(),
            working_time: Some(ProcessWorkingTimeSummary {
                calendar_name: "Office".into(),
                holiday_policy: HolidayPolicy::PolandStatutory,
                pin_sha256: "calendar-pin-sha".into(),
                legal_release_id: "PL-statutory-2026".into(),
                legal_as_of_date: "2026-10-02".into(),
                tzdb_release_id: "2026e".into(),
                due_offset_seconds: Some(90),
            }),
            persisted_timer: None,
        },
    };
    assert_eq!(serde_json::from_str::<ProcessStartCatalogEntry>(
        &serde_json::to_string(&working_timer).unwrap(),
    ).unwrap(), working_timer);

    for entry in [message, timer] {
        let encoded = tentaflow_protocol::cbor::encode(&entry).unwrap();
        let decoded: ProcessStartCatalogEntry = tentaflow_protocol::cbor::decode(&encoded).unwrap();
        assert_eq!(decoded, entry);
        assert_eq!(tentaflow_protocol::cbor::encode(&decoded).unwrap(), encoded);
    }

    let mut timer_missing_nullable: Value = serde_json::from_str(fixture).unwrap();
    let timer_variant = timer_missing_nullable["trigger"]["TimerStart"].as_object_mut().unwrap();
    timer_variant.remove("working_time");
    assert!(serde_json::from_value::<ProcessStartCatalogEntry>(timer_missing_nullable.clone()).is_err());
    assert!(tentaflow_protocol::cbor::decode::<ProcessStartCatalogEntry>(
        &tentaflow_protocol::cbor::encode(&timer_missing_nullable).unwrap(),
    ).is_err());

    let mut timer_missing_summary: Value = serde_json::from_str(fixture).unwrap();
    timer_missing_summary["trigger"]["TimerStart"].as_object_mut().unwrap().remove("persisted_timer");
    assert!(serde_json::from_value::<ProcessStartCatalogEntry>(timer_missing_summary.clone()).is_err());
    assert!(tentaflow_protocol::cbor::decode::<ProcessStartCatalogEntry>(
        &tentaflow_protocol::cbor::encode(&timer_missing_summary).unwrap(),
    ).is_err());
}

#[test]
fn historical_authored_published_model_json_and_cbor_bytes_remain_exact() {
    let model_json = include_str!("fixtures/model.json");
    let model_cbor = include_bytes!("fixtures/model.cbor");
    let call_json = include_str!("fixtures/call.json");
    let call_cbor = include_bytes!("fixtures/call.cbor");

    let model: ProcessModel = serde_json::from_str(model_json).unwrap();
    assert_eq!(serde_json::to_string(&model).unwrap().as_bytes(), model_json.as_bytes());
    assert_eq!(tentaflow_protocol::cbor::encode(&model).unwrap(), model_cbor);
    assert_eq!(
        tentaflow_protocol::cbor::decode::<ProcessModel>(model_cbor).unwrap(),
        model
    );

    let call: ProcessNodeKind = serde_json::from_str(call_json).unwrap();
    assert_eq!(serde_json::to_string(&call).unwrap().as_bytes(), call_json.as_bytes());
    assert_eq!(tentaflow_protocol::cbor::encode(&call).unwrap(), call_cbor);
    assert_eq!(
        tentaflow_protocol::cbor::decode::<ProcessNodeKind>(call_cbor).unwrap(),
        call
    );
}

#[test]
fn simulation_payloads_round_trip_by_variant_name_through_json_and_cbor() {
    use tentaflow_protocol::message_body::MessageBody;
    use tentaflow_protocol::processes::{
        ProcessPayload, ProcessSimulationClock, ProcessSimulationEvent, ProcessSimulationSource,
        ProcessSimulationTraceStep, ProcessSimulationView,
    };

    let view = ProcessSimulationView {
        simulation_id: "simulation-1".into(),
        source: ProcessSimulationSource {
            simulation_id: "simulation-1".into(),
            definition_id: "definition-1".into(),
            version: 3,
            model_sha256: "a".repeat(64),
            selected_process_id: "Process_1".into(),
            start_node_id: "Start_1".into(),
        },
        clock: ProcessSimulationClock {
            start_ms: 1_000, now_ms: 1_100, horizon_ms: 9_000,
            tick_duration_ms: 100, step_index: 1, revision: 2,
        },
        instance: None,
        user_tasks: Vec::new(),
        timers: Vec::new(),
        incidents: Vec::new(),
        events: vec![ProcessSimulationEvent {
            event_id: "event-1".into(), seq: 1, at_ms: 1_000, kind: "instance_started".into(),
            node_id: None, actor_user_id: Some("user-1".into()), data: json!({}),
            scope_id: "scope-1".into(),
        }],
        trace_steps: vec![ProcessSimulationTraceStep {
            trace_step_id: "trace-1".into(), ordinal: 0, action: "start".into(), at_ms: 1_000,
            request_sha256: "b".repeat(64), result_sha256: "c".repeat(64), data: json!({}),
        }],
        activity_io_witnesses: vec![json!({"phase": "output_applied"})],
    };
    let payloads = [
        json!({"SimulationStartRequest": {"definition_id": "definition-1", "version": 3,
            "selected_process_id": "Process_1", "start_node_id": "Start_1",
            "variables": {"value": null}, "start_ms": 1_000, "horizon_ms": 9_000,
            "tick_duration_ms": 100}}),
        json!({"SimulationViewRequest": {"simulation_id": "simulation-1"}}),
        json!({"SimulationAdvanceRequest": {"simulation_id": "simulation-1"}}),
        json!({"SimulationUserTaskCompleteRequest": {"simulation_id": "simulation-1",
            "user_task_id": "task-1", "outputs": {"decision": "yes"}}}),
        json!({"SimulationManualTaskAcknowledgeRequest": {"simulation_id": "simulation-1",
            "user_task_id": "task-1"}}),
        json!({"SimulationReleaseRequest": {"simulation_id": "simulation-1"}}),
        json!({"SimulationReleaseResponse": {"simulation_id": "simulation-1"}}),
        json!({"SimulationStartResponse": {"view": serde_json::to_value(&view).unwrap()}}),
        json!({"SimulationViewResponse": {"view": serde_json::to_value(&view).unwrap()}}),
        json!({"SimulationAdvanceResponse": {"view": serde_json::to_value(&view).unwrap()}}),
        json!({"SimulationUserTaskCompleteResponse": {"view": serde_json::to_value(&view).unwrap()}}),
        json!({"SimulationManualTaskAcknowledgeResponse": {"view": serde_json::to_value(&view).unwrap()}}),
    ];
    for expected in payloads {
        let payload: ProcessPayload = serde_json::from_value(expected.clone()).unwrap();
        assert_eq!(serde_json::to_value(&payload).unwrap(), expected);
        let body = MessageBody::ProcessBody(payload);
        let bytes = tentaflow_protocol::cbor::encode(&body).unwrap();
        assert_eq!(tentaflow_protocol::cbor::decode::<MessageBody>(&bytes).unwrap(), body);
    }
}
