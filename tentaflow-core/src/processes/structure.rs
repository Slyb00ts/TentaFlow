// ============ File: structure.rs — allow-list structural projection of a pinned process model for non-owner readers ============

use std::collections::BTreeMap;

use tentaflow_protocol::processes::{
    ActivityVerification, ProcessCallActivity, ProcessCallTarget, ProcessCallableReference,
    ProcessErrorDeclaration, ProcessEscalationDeclaration, ProcessExecutableProcess,
    ProcessMessageTargetSpec, ProcessModel, ProcessMultiInstanceInput, ProcessNode,
    ProcessNodeKind, ProcessRepeatSpec, ProcessSequenceFlow, ProcessSignalDeclaration,
    ProcessSubProcess, ProcessTimerSpec, ServiceImplementation,
};

/// What a reader who does not own the definition may see of its pinned model:
/// element ids, names, kinds, nesting, repetition kind, boundary hosts, flow
/// endpoints and the diagram geometry. Everything an author wrote to make the
/// process run (expressions, scripts, mappings, service and agent settings,
/// conditions, correlation, timer values, assignees, calendars, data objects,
/// annotations) is replaced by a neutral value.
///
/// The projection is an allow-list: every output field is built here from a
/// named source, and `ProcessNodeKind` is matched without a wildcard, so a new
/// kind cannot reach a reader before someone decides what of it is structural.
pub fn structural_projection(model: &ProcessModel) -> ProcessModel {
    ProcessModel {
        schema_version: model.schema_version,
        process_id: model.process_id.clone(),
        nodes: project_nodes(&model.nodes),
        sequence_flows: project_flows(&model.sequence_flows),
        variables: BTreeMap::new(),
        diagram: project_diagram(&model.diagram),
        timer_timezone: None,
        work_calendar: None,
        calendar_pin: None,
        messages: model.messages.iter().map(|message| {
            tentaflow_protocol::processes::ProcessMessageDeclaration {
                message_id: message.message_id.clone(),
                name: message.name.clone(),
            }
        }).collect(),
        errors: model.errors.iter().map(|error| ProcessErrorDeclaration {
            error_id: error.error_id.clone(),
            name: error.name.clone(),
            error_code: String::new(),
        }).collect(),
        target_namespace: None,
        escalations: model.escalations.iter().map(|escalation| ProcessEscalationDeclaration {
            escalation_id: escalation.escalation_id.clone(),
            name: escalation.name.clone(),
            escalation_code: String::new(),
        }).collect(),
        signals: model.signals.iter().map(|signal| ProcessSignalDeclaration {
            signal_id: signal.signal_id.clone(),
            namespace_uri: String::new(),
            name: signal.name.clone(),
        }).collect(),
        process_name: model.process_name.clone(),
        additional_processes: model.additional_processes.iter().map(|process| {
            ProcessExecutableProcess {
                process_id: process.process_id.clone(),
                process_name: process.process_name.clone(),
                nodes: project_nodes(&process.nodes),
                sequence_flows: project_flows(&process.sequence_flows),
                variables: BTreeMap::new(),
                diagram: project_diagram(&process.diagram),
                timer_timezone: None,
                work_calendar: None,
                calendar_pin: None,
                modeling: None,
            }
        }).collect(),
        modeling: None,
        collaboration: None,
        data_stores: Vec::new(),
    }
}

fn project_nodes(nodes: &[ProcessNode]) -> Vec<ProcessNode> {
    nodes.iter().map(|node| ProcessNode {
        id: node.id.clone(),
        name: node.name.clone(),
        kind: project_kind(&node.kind),
        repeat: node.repeat.as_ref().map(project_repeat),
        activity_io: None,
    }).collect()
}

fn project_flows(flows: &[ProcessSequenceFlow]) -> Vec<ProcessSequenceFlow> {
    flows.iter().map(|flow| ProcessSequenceFlow {
        id: flow.id.clone(),
        source_id: flow.source_id.clone(),
        target_id: flow.target_id.clone(),
        condition: None,
        call_start_node_id: flow.call_start_node_id.clone(),
    }).collect()
}

fn project_diagram(diagram: &tentaflow_protocol::processes::ProcessDiagram) -> tentaflow_protocol::processes::ProcessDiagram {
    tentaflow_protocol::processes::ProcessDiagram {
        shapes: diagram.shapes.iter().map(|shape| tentaflow_protocol::processes::ProcessShape {
            element_id: shape.element_id.clone(),
            x: shape.x,
            y: shape.y,
            width: shape.width,
            height: shape.height,
        }).collect(),
        edges: diagram.edges.iter().map(|edge| tentaflow_protocol::processes::ProcessEdgeDiagram {
            sequence_flow_id: edge.sequence_flow_id.clone(),
            waypoints: edge.waypoints.iter().map(|point| tentaflow_protocol::processes::ProcessPoint {
                x: point.x,
                y: point.y,
            }).collect(),
        }).collect(),
        modeling_shapes: Vec::new(),
        modeling_edges: Vec::new(),
    }
}

/// Only the repetition kind and the way it runs survive; the cardinality, the
/// collection expression, the loop condition and the output variable do not.
fn project_repeat(repeat: &ProcessRepeatSpec) -> ProcessRepeatSpec {
    match repeat {
        ProcessRepeatSpec::MultiInstance { mode, .. } => ProcessRepeatSpec::MultiInstance {
            mode: *mode,
            input: ProcessMultiInstanceInput::Cardinality { count: 1 },
            output_collection_variable: String::new(),
        },
        ProcessRepeatSpec::StructuredLoop { .. } => ProcessRepeatSpec::StructuredLoop {
            condition: String::new(),
            test_before: true,
            max_iterations: 1,
            output_collection_variable: String::new(),
        },
    }
}

fn no_timer() -> ProcessTimerSpec {
    ProcessTimerSpec::Duration { seconds: 0 }
}

fn no_target() -> ProcessMessageTargetSpec {
    ProcessMessageTargetSpec::Catch {
        definition_id: String::new(),
        instance_id_expression: None,
        subscription_id_expression: None,
    }
}

fn project_kind(kind: &ProcessNodeKind) -> ProcessNodeKind {
    use ProcessNodeKind as K;
    match kind {
        K::Start => K::Start,
        K::End => K::End,
        K::TerminateEnd => K::TerminateEnd,
        K::ParallelGateway => K::ParallelGateway,
        K::EventBasedGateway => K::EventBasedGateway,
        K::UserTask { .. } => K::UserTask {
            assignee_user_id: None,
            output_mapping: BTreeMap::new(),
        },
        K::ServiceTask { .. } => K::ServiceTask {
            flow_id: String::new(),
            input_mapping: BTreeMap::new(),
            output_mapping: BTreeMap::new(),
            verification: ActivityVerification::Human,
            timeout_seconds: 0,
            result_expression: None,
            implementation: ServiceImplementation::Flow,
        },
        K::ExclusiveGateway { default_flow_id } => K::ExclusiveGateway {
            default_flow_id: default_flow_id.clone(),
        },
        K::InclusiveGateway { default_flow_id } => K::InclusiveGateway {
            default_flow_id: default_flow_id.clone(),
        },
        K::TimerStart { .. } => K::TimerStart { timer: no_timer() },
        K::TimerCatch { .. } => K::TimerCatch { timer: no_timer() },
        K::BoundaryTimer { attached_to_id, cancel_activity, .. } => K::BoundaryTimer {
            attached_to_id: attached_to_id.clone(),
            cancel_activity: *cancel_activity,
            timer: no_timer(),
        },
        K::MessageStart { message_ref, .. } => K::MessageStart {
            message_ref: message_ref.clone(),
            output_mapping: BTreeMap::new(),
        },
        K::MessageCatch { message_ref, .. } => K::MessageCatch {
            message_ref: message_ref.clone(),
            correlation_expression: String::new(),
            output_mapping: BTreeMap::new(),
        },
        K::MessageThrow { message_ref, .. } => K::MessageThrow {
            message_ref: message_ref.clone(),
            target: no_target(),
            correlation_expression: String::new(),
            payload_expression: String::new(),
            ttl_seconds: 0,
        },
        K::BoundaryMessage { attached_to_id, cancel_activity, message_ref, .. } => K::BoundaryMessage {
            attached_to_id: attached_to_id.clone(),
            cancel_activity: *cancel_activity,
            message_ref: message_ref.clone(),
            correlation_expression: String::new(),
            output_mapping: BTreeMap::new(),
        },
        K::BoundaryError { attached_to_id, error_ref, .. } => K::BoundaryError {
            attached_to_id: attached_to_id.clone(),
            error_ref: error_ref.clone(),
            output_mapping: BTreeMap::new(),
        },
        K::BoundaryEscalation { attached_to_id, escalation_ref, cancel_activity, .. } => K::BoundaryEscalation {
            attached_to_id: attached_to_id.clone(),
            escalation_ref: escalation_ref.clone(),
            cancel_activity: *cancel_activity,
            output_mapping: BTreeMap::new(),
        },
        K::SubProcess { body, .. } => K::SubProcess {
            body: ProcessSubProcess {
                nodes: project_nodes(&body.nodes),
                sequence_flows: project_flows(&body.sequence_flows),
                variables: BTreeMap::new(),
                diagram: project_diagram(&body.diagram),
                modeling: None,
            },
            input_mapping: BTreeMap::new(),
            output_mapping: BTreeMap::new(),
        },
        K::CallActivity(_) => K::CallActivity(ProcessCallActivity {
            target: ProcessCallTarget::LocalBody {
                called_element: ProcessCallableReference {
                    namespace_uri: String::new(),
                    process_id: String::new(),
                },
            },
            input_mapping: BTreeMap::new(),
            output_mapping: BTreeMap::new(),
        }),
        K::ErrorEnd { error_ref } => K::ErrorEnd { error_ref: error_ref.clone() },
        K::ScriptTask { .. } => K::ScriptTask {
            script: String::new(),
            output_mapping: BTreeMap::new(),
        },
        K::ManualTask { .. } => K::ManualTask {
            assignee_user_id: None,
            instructions: String::new(),
        },
        K::SendTask { message_ref, .. } => K::SendTask {
            message_ref: message_ref.clone(),
            target: no_target(),
            correlation_expression: String::new(),
            payload_expression: String::new(),
            ttl_seconds: 0,
        },
        K::ReceiveTask { message_ref, .. } => K::ReceiveTask {
            message_ref: message_ref.clone(),
            correlation_expression: String::new(),
            output_mapping: BTreeMap::new(),
        },
        K::SignalThrow { signal_ref, .. } => K::SignalThrow {
            signal_ref: signal_ref.clone(),
            payload_expression: String::new(),
            ttl_seconds: 0,
        },
        K::SignalCatch { signal_ref, .. } => K::SignalCatch {
            signal_ref: signal_ref.clone(),
            output_mapping: BTreeMap::new(),
        },
        K::LinkThrow { definition } => K::LinkThrow { definition: definition.clone() },
        K::LinkCatch { definition } => K::LinkCatch { definition: definition.clone() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tentaflow_protocol::processes::{
        ProcessDiagram, ProcessEdgeDiagram, ProcessLinkEventDefinition,
        ProcessMessageDeclaration, ProcessMultiInstanceMode, ProcessPoint, ProcessShape,
        ProcessWorkCalendar,
    };

    const SECRET: &str = "SECRET-LEAK";

    fn secret_map() -> BTreeMap<String, String> {
        BTreeMap::from([(format!("{SECRET}-key"), format!("{SECRET}-value"))])
    }

    fn node(id: &str, kind: ProcessNodeKind) -> ProcessNode {
        ProcessNode {
            id: id.into(),
            name: format!("Visible {id}"),
            kind,
            repeat: None,
            activity_io: None,
        }
    }

    /// One node of every kind, with the secret planted in every field a reader
    /// must not see. Ids and names carry no marker: they are the allowed part.
    fn model_with_secrets() -> ProcessModel {
        use ProcessNodeKind as K;
        let secret = || SECRET.to_string();
        let timer = || ProcessTimerSpec::Date { at: format!("2031-01-01T00:00:00Z{SECRET}") };
        let target = || ProcessMessageTargetSpec::Start {
            definition_id: secret(),
            process_id: Some(secret()),
            start_node_id: Some(secret()),
        };
        let link = || ProcessLinkEventDefinition {
            id: "link-1".into(),
            name: "Visible link".into(),
            source_refs: vec!["Link".into()],
            target_ref: Some("Link".into()),
        };
        let body = ProcessSubProcess {
            nodes: vec![node("Inner", K::ScriptTask { script: secret(), output_mapping: secret_map() })],
            sequence_flows: vec![ProcessSequenceFlow {
                id: "InnerFlow".into(), source_id: "Inner".into(), target_id: "Inner".into(),
                condition: Some(secret()), call_start_node_id: None,
            }],
            variables: BTreeMap::from([(secret(), json!(secret()))]),
            diagram: ProcessDiagram::default(),
            modeling: Some(Default::default()),
        };
        let mut looped = node("Looped", K::ScriptTask { script: secret(), output_mapping: secret_map() });
        looped.repeat = Some(ProcessRepeatSpec::StructuredLoop {
            condition: secret(), test_before: false, max_iterations: 9,
            output_collection_variable: secret(),
        });
        let mut parallel = node("Parallel", K::ManualTask {
            assignee_user_id: Some(secret()), instructions: secret(),
        });
        parallel.repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::CollectionExpression { expression: secret() },
            output_collection_variable: secret(),
        });
        let mut nodes = vec![
            node("Start", K::Start),
            node("End", K::End),
            node("User", K::UserTask { assignee_user_id: Some(secret()), output_mapping: secret_map() }),
            node("Service", K::ServiceTask {
                flow_id: secret(), input_mapping: secret_map(), output_mapping: secret_map(),
                verification: ActivityVerification::Critic {
                    agent_id: secret(), criteria: secret(), on_failure: Default::default(),
                },
                timeout_seconds: 77, result_expression: Some(secret()),
                implementation: ServiceImplementation::Agent {
                    agent_id: secret(), goal_expression: secret(), allowed_tools: vec![secret()],
                    max_iterations: 5, output_schema: Some(json!({"secret": secret()})),
                },
            }),
            node("Tool", K::ServiceTask {
                flow_id: secret(), input_mapping: secret_map(), output_mapping: secret_map(),
                verification: ActivityVerification::Condition { expression: secret() },
                timeout_seconds: 5, result_expression: None,
                implementation: ServiceImplementation::AddonTool { addon_id: secret(), tool_name: secret() },
            }),
            node("Block", K::ServiceTask {
                flow_id: secret(), input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
                verification: ActivityVerification::Test { flow_id: secret(), on_failure: Default::default() },
                timeout_seconds: 5, result_expression: None,
                implementation: ServiceImplementation::Block { node_type: secret(), config: json!({"token": secret()}) },
            }),
            node("Unbound", K::ServiceTask {
                flow_id: secret(), input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
                verification: ActivityVerification::Human, timeout_seconds: 5, result_expression: None,
                implementation: ServiceImplementation::Unbound { origin: secret() },
            }),
            node("Xor", K::ExclusiveGateway { default_flow_id: Some("Flow".into()) }),
            node("Or", K::InclusiveGateway { default_flow_id: None }),
            node("And", K::ParallelGateway),
            node("EventGate", K::EventBasedGateway),
            node("TimerStart", K::TimerStart { timer: timer() }),
            node("TimerCatch", K::TimerCatch { timer: timer() }),
            node("BoundaryTimer", K::BoundaryTimer { attached_to_id: "User".into(), cancel_activity: true, timer: timer() }),
            node("MessageStart", K::MessageStart { message_ref: "Msg".into(), output_mapping: secret_map() }),
            node("MessageCatch", K::MessageCatch {
                message_ref: "Msg".into(), correlation_expression: secret(), output_mapping: secret_map(),
            }),
            node("MessageThrow", K::MessageThrow {
                message_ref: "Msg".into(), target: target(), correlation_expression: secret(),
                payload_expression: secret(), ttl_seconds: 11,
            }),
            node("BoundaryMessage", K::BoundaryMessage {
                attached_to_id: "User".into(), cancel_activity: false, message_ref: "Msg".into(),
                correlation_expression: secret(), output_mapping: secret_map(),
            }),
            node("BoundaryError", K::BoundaryError {
                attached_to_id: "Service".into(), error_ref: Some("Err".into()), output_mapping: secret_map(),
            }),
            node("BoundaryEscalation", K::BoundaryEscalation {
                attached_to_id: "Sub".into(), escalation_ref: Some("Esc".into()), cancel_activity: true,
                output_mapping: secret_map(),
            }),
            node("Sub", K::SubProcess { body, input_mapping: secret_map(), output_mapping: secret_map() }),
            node("PublishedCall", K::CallActivity(ProcessCallActivity {
                target: ProcessCallTarget::PublishedBody {
                    definition_id: secret(), version: 4,
                    called_element: ProcessCallableReference { namespace_uri: secret(), process_id: secret() },
                },
                input_mapping: secret_map(), output_mapping: secret_map(),
            })),
            node("LocalCall", K::CallActivity(ProcessCallActivity {
                target: ProcessCallTarget::LocalBody {
                    called_element: ProcessCallableReference { namespace_uri: secret(), process_id: secret() },
                },
                input_mapping: secret_map(), output_mapping: secret_map(),
            })),
            node("ErrorEnd", K::ErrorEnd { error_ref: "Err".into() }),
            node("Terminate", K::TerminateEnd),
            node("Script", K::ScriptTask { script: secret(), output_mapping: secret_map() }),
            node("Manual", K::ManualTask { assignee_user_id: Some(secret()), instructions: secret() }),
            node("Send", K::SendTask {
                message_ref: "Msg".into(), target: target(), correlation_expression: secret(),
                payload_expression: secret(), ttl_seconds: 3,
            }),
            node("Receive", K::ReceiveTask {
                message_ref: "Msg".into(), correlation_expression: secret(), output_mapping: secret_map(),
            }),
            node("SignalThrow", K::SignalThrow {
                signal_ref: "Sig".into(), payload_expression: secret(), ttl_seconds: 3,
            }),
            node("SignalCatch", K::SignalCatch { signal_ref: "Sig".into(), output_mapping: secret_map() }),
            node("LinkThrow", K::LinkThrow { definition: link() }),
            node("LinkCatch", K::LinkCatch { definition: link() }),
            looped,
            parallel,
        ];
        nodes[2].activity_io = Some(tentaflow_protocol::processes::ProcessActivityIo {
            data_inputs: Vec::new(),
            data_outputs: vec![tentaflow_protocol::processes::ProcessIoDataOutput {
                id: "out".into(), name: None, value_expression: secret(),
            }],
            input_set_id: "is".into(), input_set: Vec::new(),
            output_set_id: "os".into(), output_set: vec!["out".into()],
            input_associations: vec![
                tentaflow_protocol::processes::ProcessInputAssociation::CelAssignment {
                    id: "ia".into(), from_expression: secret(), target_input_id: "in".into(),
                },
            ],
            output_associations: Vec::new(),
            coordinator_output: None,
        });
        ProcessModel {
            schema_version: 1,
            process_id: "Process_1".into(),
            nodes,
            sequence_flows: vec![ProcessSequenceFlow {
                id: "Flow".into(), source_id: "Start".into(), target_id: "End".into(),
                condition: Some(format!("amount > 1000 && token == '{SECRET}'")),
                call_start_node_id: Some("Start".into()),
            }],
            variables: BTreeMap::from([("customer".into(), json!(secret()))]),
            diagram: ProcessDiagram {
                shapes: vec![ProcessShape { element_id: "Start".into(), x: 10.0, y: 20.0, width: 36.0, height: 36.0 }],
                edges: vec![ProcessEdgeDiagram {
                    sequence_flow_id: "Flow".into(),
                    waypoints: vec![ProcessPoint { x: 1.0, y: 2.0 }, ProcessPoint { x: 3.0, y: 4.0 }],
                }],
                modeling_shapes: vec![tentaflow_protocol::processes::ProcessModelingShape {
                    di_id: secret(), element_id: secret(), x: 0.0, y: 0.0, width: 1.0, height: 1.0,
                }],
                modeling_edges: Vec::new(),
            },
            timer_timezone: Some(secret()),
            work_calendar: Some(ProcessWorkCalendar {
                name: secret(),
                weekly_windows: Vec::new(),
                manual_days_off: vec![tentaflow_protocol::processes::ManualDayOff {
                    date: "2031-01-01".into(), reason: secret(),
                }],
                holiday_policy: tentaflow_protocol::processes::HolidayPolicy::None,
            }),
            calendar_pin: None,
            messages: vec![ProcessMessageDeclaration { message_id: "Msg".into(), name: "Visible message".into() }],
            errors: vec![ProcessErrorDeclaration { error_id: "Err".into(), name: "Visible error".into(), error_code: secret() }],
            target_namespace: Some(secret()),
            escalations: vec![ProcessEscalationDeclaration {
                escalation_id: "Esc".into(), name: "Visible escalation".into(), escalation_code: secret(),
            }],
            signals: vec![ProcessSignalDeclaration {
                signal_id: "Sig".into(), namespace_uri: secret(), name: "Visible signal".into(),
            }],
            process_name: Some("Visible process".into()),
            additional_processes: vec![ProcessExecutableProcess {
                process_id: "Process_2".into(),
                process_name: Some("Visible second".into()),
                nodes: vec![node("Other", K::ScriptTask { script: secret(), output_mapping: secret_map() })],
                sequence_flows: vec![ProcessSequenceFlow {
                    id: "OtherFlow".into(), source_id: "Other".into(), target_id: "Other".into(),
                    condition: Some(secret()), call_start_node_id: None,
                }],
                variables: BTreeMap::from([(secret(), json!(secret()))]),
                diagram: ProcessDiagram::default(),
                timer_timezone: Some(secret()),
                work_calendar: None,
                calendar_pin: None,
                modeling: Some(Default::default()),
            }],
            modeling: Some(Default::default()),
            collaboration: None,
            data_stores: vec![tentaflow_protocol::processes::ProcessDataStore {
                id: secret(), name: Some(secret()), capacity: None, is_unlimited: None,
            }],
        }
    }

    #[test]
    fn no_authored_secret_survives_in_any_field_of_the_projection() {
        let model = model_with_secrets();
        let original = serde_json::to_string(&model).unwrap();
        assert!(original.matches(SECRET).count() > 60, "the fixture must plant the secret widely");

        let projected = structural_projection(&model);
        let wire = serde_json::to_string(&projected).unwrap();
        assert!(!wire.contains(SECRET), "leaked: {wire}");
        assert!(!wire.contains("amount > 1000"));
        assert!(!wire.contains("customer"));
        // The cbor wire form is what actually reaches the reader.
        let cbor = tentaflow_protocol::cbor::encode(&projected).unwrap();
        assert!(!cbor.windows(SECRET.len()).any(|window| window == SECRET.as_bytes()));
    }

    #[test]
    fn the_structure_a_diagram_needs_survives() {
        let model = model_with_secrets();
        let projected = structural_projection(&model);

        let ids = |nodes: &[ProcessNode]| nodes.iter().map(|node| (node.id.clone(), node.name.clone())).collect::<Vec<_>>();
        assert_eq!(ids(&projected.nodes), ids(&model.nodes));
        assert_eq!(projected.sequence_flows.len(), 1);
        assert_eq!(projected.sequence_flows[0].source_id, "Start");
        assert_eq!(projected.sequence_flows[0].target_id, "End");
        assert_eq!(projected.sequence_flows[0].condition, None);
        assert_eq!(projected.diagram.shapes, model.diagram.shapes);
        assert_eq!(projected.diagram.edges, model.diagram.edges);
        assert_eq!(projected.process_name.as_deref(), Some("Visible process"));
        assert_eq!(projected.additional_processes[0].nodes[0].id, "Other");
        assert_eq!(projected.messages[0].name, "Visible message");

        let kind_name = |kind: &ProcessNodeKind| match serde_json::to_value(kind).unwrap() {
            serde_json::Value::String(name) => name,
            serde_json::Value::Object(map) => map.keys().next().unwrap().clone(),
            other => panic!("unexpected kind {other}"),
        };
        for (before, after) in model.nodes.iter().zip(&projected.nodes) {
            assert_eq!(kind_name(&before.kind), kind_name(&after.kind), "{}", before.id);
        }
        let by_id = |id: &str| projected.nodes.iter().find(|node| node.id == id).unwrap();
        assert!(matches!(&by_id("BoundaryTimer").kind,
            ProcessNodeKind::BoundaryTimer { attached_to_id, cancel_activity: true, .. } if attached_to_id == "User"));
        assert!(matches!(&by_id("Xor").kind,
            ProcessNodeKind::ExclusiveGateway { default_flow_id: Some(flow) } if flow == "Flow"));
        let ProcessNodeKind::SubProcess { body, .. } = &by_id("Sub").kind else { panic!("sub-process") };
        assert_eq!(body.nodes[0].id, "Inner");
        assert!(matches!(by_id("Looped").repeat, Some(ProcessRepeatSpec::StructuredLoop { .. })));
        assert!(matches!(by_id("Parallel").repeat,
            Some(ProcessRepeatSpec::MultiInstance { mode: ProcessMultiInstanceMode::Parallel, .. })));
    }
}
