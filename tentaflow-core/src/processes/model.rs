// ============ File: model.rs — Process graph validation and structured gateway joins ============

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use tentaflow_protocol::processes::{
    ProcessDiagram, ProcessEscalationPathReason, ProcessMessageTargetSpec, ProcessModel,
    ProcessMultiInstanceInput, ProcessNode, ProcessNodeKind, ProcessRepeatSpec,
    ProcessSequenceFlow, ProcessTimerSpec,
};

use crate::flow_engine::expr;
use crate::project_studio::schedules::parse_timezone;

pub const MAX_MODEL_BYTES: usize = 512 * 1024;
pub const MAX_NODES: usize = 128;
pub const MAX_SEQUENCE_FLOWS: usize = 256;
pub const MAX_VARIABLE_BYTES: usize = 256 * 1024;
pub const MAX_VARIABLE_KEYS: usize = 128;
const MAX_DI_COORDINATE: f64 = 1_000_000.0;

fn valid_manual_instructions(value: &str) -> bool {
    value.len() <= 4096 && value.chars().all(|ch| {
        matches!(ch, '\t' | '\n' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayKind {
    Parallel,
    Inclusive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayBranchExits {
    pub join_incoming_edge_id: Option<String>,
    pub terminate_end_node_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayPair {
    pub kind: GatewayKind,
    pub split_node_id: String,
    pub join_node_id: Option<String>,
    pub branches: BTreeMap<String, GatewayBranchExits>,
}

pub fn scope_body<'a>(
    model: &'a ProcessModel,
    subprocess_node_ids: &[String],
) -> Result<(&'a [ProcessNode], &'a [ProcessSequenceFlow], &'a BTreeMap<String, serde_json::Value>)> {
    let mut nodes = model.nodes.as_slice();
    let mut flows = model.sequence_flows.as_slice();
    let mut variables = &model.variables;
    for node_id in subprocess_node_ids {
        let node = nodes.iter().find(|node| node.id == *node_id)
            .with_context(|| format!("subprocess {node_id} is outside its parent body"))?;
        let ProcessNodeKind::SubProcess { body, .. } = &node.kind else {
            bail!("scope path node {node_id} is not a subprocess");
        };
        nodes = &body.nodes;
        flows = &body.sequence_flows;
        variables = &body.variables;
    }
    Ok((nodes, flows, variables))
}

pub fn all_nodes(model: &ProcessModel) -> Vec<&ProcessNode> {
    fn visit<'a>(nodes: &'a [ProcessNode], result: &mut Vec<&'a ProcessNode>) {
        for node in nodes {
            result.push(node);
            if let ProcessNodeKind::SubProcess { body, .. } = &node.kind {
                visit(&body.nodes, result);
            }
        }
    }
    let mut nodes = Vec::new();
    visit(&model.nodes, &mut nodes);
    nodes
}

pub fn starter_model() -> ProcessModel {
    use tentaflow_protocol::processes::{ProcessDiagram, ProcessNode, ProcessSequenceFlow};
    ProcessModel {
        schema_version: 1,
        process_id: "Process_1".into(),
        nodes: vec![
            ProcessNode { repeat: None,
                id: "Start_1".into(),
                name: "Start".into(),
                kind: ProcessNodeKind::Start,
            },
            ProcessNode { repeat: None,
                id: "End_1".into(),
                name: "End".into(),
                kind: ProcessNodeKind::End,
            },
        ],
        sequence_flows: vec![ProcessSequenceFlow {
            id: "Flow_1".into(),
            source_id: "Start_1".into(),
            target_id: "End_1".into(),
            condition: None,
        }],
        variables: Default::default(),
        diagram: ProcessDiagram::default(),
        timer_timezone: None,
        work_calendar: None,
        calendar_pin: None,
        messages: Vec::new(),
        errors: Vec::new(),
        target_namespace: None,
        escalations: Vec::new(),
        signals: Vec::new(),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.as_bytes()[0].is_ascii_alphabetic()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn validate_declarations<'a>(model: &'a ProcessModel, ids: &mut HashSet<&'a str>) -> Result<()> {
    if let Some(namespace) = &model.target_namespace {
        ensure!(
            !namespace.is_empty()
                && namespace.len() <= 1024
                && !namespace.chars().any(char::is_whitespace)
                && !namespace.chars().any(char::is_control),
            "invalid process target namespace"
        );
        let parsed = url::Url::parse(namespace).context("invalid process target namespace")?;
        ensure!(
            !parsed.scheme().is_empty(),
            "process namespace must be absolute"
        );
    }
    ensure!(
        model.messages.len() <= 32 && model.errors.len() <= 32
            && model.escalations.len() <= 32 && model.signals.len() <= 32,
        "process declaration limit exceeded"
    );
    let mut names = HashSet::new();
    for message in &model.messages {
        ensure!(
            valid_id(&message.message_id) && ids.insert(message.message_id.as_str()),
            "invalid or duplicate BPMN ID: {}",
            message.message_id
        );
        ensure!(
            !message.name.is_empty()
                && message.name.len() <= 256
                && !message.name.chars().any(char::is_control)
                && names.insert(message.name.as_str()),
            "invalid or duplicate message name: {}",
            message.name
        );
    }
    let mut codes = HashSet::new();
    for error in &model.errors {
        ensure!(
            valid_id(&error.error_id) && ids.insert(error.error_id.as_str()),
            "invalid or duplicate BPMN ID: {}",
            error.error_id
        );
        ensure!(
            !error.name.is_empty()
                && error.name.len() <= 256
                && !error.name.chars().any(char::is_control),
            "invalid error name: {}",
            error.error_id
        );
        ensure!(
            (1..=64).contains(&error.error_code.len())
                && error
                    .error_code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
                && codes.insert(error.error_code.as_str()),
            "invalid or duplicate error code: {}",
            error.error_code
        );
    }
    let mut escalation_codes = HashSet::new();
    for escalation in &model.escalations {
        ensure!(
            valid_id(&escalation.escalation_id) && ids.insert(escalation.escalation_id.as_str()),
            "invalid or duplicate BPMN ID: {}",
            escalation.escalation_id
        );
        ensure!(
            !escalation.name.is_empty()
                && escalation.name.len() <= 256
                && !escalation.name.chars().any(char::is_control),
            "invalid escalation name: {}",
            escalation.escalation_id
        );
        ensure!(
            (1..=64).contains(&escalation.escalation_code.len())
                && escalation
                    .escalation_code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric()
                        || matches!(byte, b'_' | b'.' | b':' | b'-'))
                && escalation_codes.insert(escalation.escalation_code.as_str()),
            "invalid or duplicate escalation code: {}",
            escalation.escalation_code
        );
    }
    if !model.signals.is_empty() {
        ensure!(model.target_namespace.is_some(), "signal declarations require an explicit target namespace");
    }
    for signal in &model.signals {
        ensure!(valid_id(&signal.signal_id) && ids.insert(signal.signal_id.as_str()),
            "invalid or duplicate BPMN ID: {}", signal.signal_id);
        ensure!(signal.namespace_uri == model.target_namespace.as_deref().unwrap_or_default(),
            "signal {} namespace differs from the process target namespace", signal.signal_id);
        ensure!(!signal.name.is_empty() && signal.name.len() <= 256
            && !signal.name.chars().any(char::is_control),
            "invalid signal name: {}", signal.signal_id);
    }
    Ok(())
}

fn validate_expression(expression: &str, context: &str, required: bool) -> Result<()> {
    if expression.is_empty() && !required {
        return Ok(());
    }
    ensure!(!expression.is_empty() && expression.len() <= 4096, "invalid {context} length");
    expr::validate_syntax(expression, None).with_context(|| context.to_string())
}

fn validate_message_target(target: &ProcessMessageTargetSpec, complete: bool) -> Result<()> {
    match target {
        ProcessMessageTargetSpec::Start { definition_id } => {
            if complete || !definition_id.is_empty() {
                uuid::Uuid::parse_str(definition_id).context("invalid message start target definition")?;
            }
        }
        ProcessMessageTargetSpec::Catch {
            definition_id,
            instance_id_expression,
            subscription_id_expression,
        } => {
            if complete || !definition_id.is_empty() {
                uuid::Uuid::parse_str(definition_id).context("invalid message catch target definition")?;
            }
            ensure!(
                subscription_id_expression.is_none() || instance_id_expression.is_some(),
                "subscription target requires an instance expression"
            );
            if let Some(expression) = instance_id_expression {
                validate_expression(expression, "message target instance expression", complete)?;
            }
            if let Some(expression) = subscription_id_expression {
                validate_expression(expression, "message target subscription expression", complete)?;
            }
        }
    }
    Ok(())
}

pub fn validate_timer_spec(spec: &ProcessTimerSpec, is_start: bool) -> Result<()> {
    match spec {
        ProcessTimerSpec::Date { at } => {
            let parsed = chrono::DateTime::parse_from_rfc3339(at)
                .with_context(|| format!("timer date must be RFC3339 with an offset: {at}"))?;
            let fractional_digits = at
                .split_once('.')
                .map(|(_, fraction)| {
                    fraction
                        .chars()
                        .take_while(|character| character.is_ascii_digit())
                        .count()
                })
                .unwrap_or(0);
            ensure!(
                fractional_digits <= 3 && parsed.timestamp_subsec_nanos() % 1_000_000 == 0,
                "timer date precision must be milliseconds or coarser"
            );
        }
        ProcessTimerSpec::Duration { seconds } => ensure!(
            (1..=31_536_000).contains(seconds),
            "timer duration must be 1..=31536000 seconds"
        ),
        ProcessTimerSpec::Cycle {
            seconds,
            total_firings,
        } => {
            ensure!(is_start, "repeating timer is only supported as a start event");
            ensure!(
                (300..=31_536_000).contains(seconds),
                "timer cycle must be 300..=31536000 seconds"
            );
            ensure!(
                total_firings.is_none_or(|count| count > 0),
                "timer firing count must be positive"
            );
        }
        ProcessTimerSpec::Daily {
            hour,
            minute,
            total_firings,
        } => {
            ensure!(is_start, "repeating timer is only supported as a start event");
            ensure!(*hour < 24 && *minute < 60, "invalid timer daily clock time");
            ensure!(
                total_firings.is_none_or(|count| count > 0),
                "timer firing count must be positive"
            );
        }
        ProcessTimerSpec::WorkingDuration { seconds } => ensure!(
            (1..=31_536_000).contains(seconds),
            "working timer duration must be 1..=31536000 seconds"
        ),
    }
    Ok(())
}

fn validate_timer_model(model: &ProcessModel) -> Result<()> {
    let has_timer = validate_timer_nodes(&model.nodes, model.work_calendar.is_some())?;
    if let Some(calendar) = &model.work_calendar {
        super::calendar::validate_work_calendar(calendar)?;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "work_calendar": calendar,
            "calendar_pin": &model.calendar_pin,
        }))?;
        ensure!(bytes.len() <= 128 * 1024, "process calendar and pin exceed 128 KiB");
    }
    super::calendar::calendar_pin_state(model)?;
    if has_timer || model.work_calendar.is_some() {
        let timezone = model
            .timer_timezone
            .as_deref()
            .context("timed process or calendar requires an explicit IANA timezone")?;
        ensure!(
            !timezone.is_empty() && timezone.trim() == timezone,
            "timer timezone must be a nonempty IANA name"
        );
        parse_timezone(timezone)?;
    } else {
        ensure!(
            model.timer_timezone.is_none(),
            "timerless process cannot declare a timer timezone"
        );
    }
    Ok(())
}

fn validate_timer_nodes(nodes: &[ProcessNode], has_calendar: bool) -> Result<bool> {
    let mut has_timer = false;
    for node in nodes {
        match &node.kind {
            ProcessNodeKind::TimerStart { timer } => {
                validate_timer_spec(timer, true)
                    .with_context(|| format!("timer start {}", node.id))?;
                if matches!(timer, ProcessTimerSpec::WorkingDuration { .. }) {
                    ensure!(has_calendar, "working timer requires a configured calendar");
                }
                has_timer = true;
            }
            ProcessNodeKind::TimerCatch { timer }
            | ProcessNodeKind::BoundaryTimer { timer, .. } => {
                validate_timer_spec(timer, false)
                    .with_context(|| format!("timer event {}", node.id))?;
                if matches!(timer, ProcessTimerSpec::WorkingDuration { .. }) {
                    ensure!(has_calendar, "working timer requires a configured calendar");
                }
                has_timer = true;
            }
            ProcessNodeKind::SubProcess { body, .. } => {
                has_timer |= validate_timer_nodes(&body.nodes, has_calendar)?;
            }
            _ => {}
        }
    }
    Ok(has_timer)
}

pub fn validate_variables(value: &serde_json::Value) -> Result<()> {
    let object = value
        .as_object()
        .context("process variables must be an object")?;
    ensure!(
        object.len() <= MAX_VARIABLE_KEYS,
        "too many process variables"
    );
    ensure!(
        serde_json::to_vec(value)?.len() <= MAX_VARIABLE_BYTES,
        "process variables exceed 256 KiB"
    );
    ensure!(
        object.keys().all(|key| valid_id(key)),
        "invalid process variable name"
    );
    Ok(())
}

fn validate_repeat(node: &ProcessNode, variables: &BTreeMap<String, serde_json::Value>, complete: bool) -> Result<()> {
    let Some(spec) = &node.repeat else { return Ok(()); };
    let output_mapping = match &node.kind {
        ProcessNodeKind::UserTask { output_mapping, .. }
        | ProcessNodeKind::ServiceTask { output_mapping, .. } => output_mapping,
        _ => bail!("repeat on {} requires a UserTask or ServiceTask", node.id),
    };
    let output_variable = match spec {
        ProcessRepeatSpec::MultiInstance { input, output_collection_variable, .. } => {
            match input {
                ProcessMultiInstanceInput::Cardinality { count } => {
                    ensure!(*count <= 16, "repeat {} cardinality exceeds 16", node.id);
                }
                ProcessMultiInstanceInput::CollectionExpression { expression } => {
                    validate_expression(expression, &format!("repeat {} collection expression", node.id), complete)?;
                }
            }
            output_collection_variable
        }
        ProcessRepeatSpec::StructuredLoop { condition, max_iterations, output_collection_variable, .. } => {
            ensure!((1..=32).contains(max_iterations), "repeat {} loop maximum outside 1..=32", node.id);
            validate_expression(condition, &format!("repeat {} loop condition", node.id), complete)?;
            output_collection_variable
        }
    };
    if complete || !output_variable.is_empty() {
        ensure!(valid_id(output_variable), "repeat {} has invalid output collection variable", node.id);
        ensure!(!output_mapping.contains_key(output_variable), "repeat {} output collection conflicts with task mapping", node.id);
        if complete {
            ensure!(variables.contains_key(output_variable), "repeat {} output collection variable is undeclared", node.id);
        }
    }
    Ok(())
}

pub fn validate_draft(model: &ProcessModel) -> Result<()> {
    ensure!(
        model.schema_version == 1 && valid_id(&model.process_id),
        "invalid process model identity"
    );
    ensure!(
        serde_json::to_vec(model)?.len() <= MAX_MODEL_BYTES,
        "process draft exceeds 512 KiB"
    );
    validate_timer_model(model)?;
    let mut all_ids = HashSet::from([model.process_id.as_str()]);
    validate_declarations(model, &mut all_ids)?;
    let mut node_count = 0;
    let mut flow_count = 0;
    validate_draft_body(
        &model.nodes,
        &model.sequence_flows,
        &model.variables,
        &model.diagram,
        0,
        &mut all_ids,
        &mut node_count,
        &mut flow_count,
    )?;
    validate_escalation_prefixes(&model.nodes, &model.sequence_flows, false)?;
    Ok(())
}

fn validate_draft_body<'a>(
    nodes: &'a [ProcessNode],
    flows: &'a [ProcessSequenceFlow],
    variables: &BTreeMap<String, serde_json::Value>,
    diagram: &ProcessDiagram,
    depth: usize,
    all_ids: &mut HashSet<&'a str>,
    node_count: &mut usize,
    flow_count: &mut usize,
) -> Result<()> {
    ensure!(depth <= 3, "embedded subprocess depth exceeds three levels");
    *node_count += nodes.len();
    *flow_count += flows.len();
    ensure!(*node_count <= MAX_NODES && *flow_count <= MAX_SEQUENCE_FLOWS,
        "process draft exceeds whole-tree graph limits");
    validate_variables(&serde_json::to_value(variables)?)?;
    for node in nodes {
        ensure!(valid_id(&node.id) && all_ids.insert(node.id.as_str()),
            "invalid or duplicate BPMN ID: {}", node.id);
        ensure!(node.name.len() <= 256 && !node.name.chars().any(char::is_control),
            "invalid node name");
        validate_repeat(node, variables, false)?;
        match &node.kind {
            ProcessNodeKind::ManualTask { assignee_user_id, instructions } => {
                ensure!(valid_manual_instructions(instructions),
                    "manual task {} has invalid instructions", node.id);
                ensure!(assignee_user_id.as_ref().map_or(true, |user| !user.is_empty()),
                    "manual task {} has empty assignee", node.id);
            }
            ProcessNodeKind::ScriptTask { script, output_mapping } => {
                ensure!(script.len() <= 4096, "script task {} expression is too long", node.id);
                if !script.is_empty() {
                    expr::validate_script_profile(script)
                        .with_context(|| format!("script task {} expression", node.id))?;
                }
                validate_mapping(output_mapping, true)?;
            }
            ProcessNodeKind::ServiceTask { input_mapping, output_mapping, verification,
                timeout_seconds, result_expression, .. } => {
                ensure!((1..=600).contains(timeout_seconds),
                    "service timeout outside 1..=600 seconds");
                validate_mapping(input_mapping, false)?;
                validate_mapping(output_mapping, false)?;
                if let tentaflow_protocol::processes::ActivityVerification::Condition { expression } = verification {
                    expr::validate_syntax(expression, None)?;
                }
                if let Some(expression) = result_expression {
                    validate_expression(expression, "service result expression", false)?;
                }
            }
            ProcessNodeKind::BoundaryEscalation { output_mapping, .. }
            | ProcessNodeKind::UserTask { output_mapping, .. }
            | ProcessNodeKind::MessageStart { output_mapping, .. }
            | ProcessNodeKind::BoundaryError { output_mapping, .. } => {
                validate_mapping(output_mapping, false)?
            }
            ProcessNodeKind::MessageCatch {
                correlation_expression,
                output_mapping,
                ..
            }
            | ProcessNodeKind::ReceiveTask {
                correlation_expression,
                output_mapping,
                ..
            }
            | ProcessNodeKind::BoundaryMessage {
                correlation_expression,
                output_mapping,
                ..
            } => {
                validate_expression(
                    correlation_expression,
                    "message correlation expression",
                    false,
                )?;
                validate_mapping(output_mapping, false)?;
            }
            ProcessNodeKind::SignalCatch { output_mapping, .. } => {
                validate_mapping(output_mapping, false)?;
            }
            ProcessNodeKind::SignalThrow { payload_expression, ttl_seconds, .. } => {
                validate_expression(payload_expression, "signal payload expression", false)?;
                ensure!((1..=604_800).contains(ttl_seconds),
                    "signal TTL outside 1..=604800 seconds");
            }
            ProcessNodeKind::MessageThrow { target, correlation_expression, payload_expression,
                ttl_seconds, .. }
            | ProcessNodeKind::SendTask { target, correlation_expression, payload_expression,
                ttl_seconds, .. } => {
                validate_message_target(target, false)?;
                validate_expression(correlation_expression, "message correlation expression", false)?;
                validate_expression(payload_expression, "message payload expression", false)?;
                ensure!((1..=604_800).contains(ttl_seconds),
                    "message TTL outside 1..=604800 seconds");
            }
            ProcessNodeKind::SubProcess { body, input_mapping, output_mapping } => {
                validate_mapping(input_mapping, false)?;
                validate_mapping(output_mapping, false)?;
                validate_draft_body(&body.nodes, &body.sequence_flows, &body.variables,
                    &body.diagram, depth + 1, all_ids, node_count, flow_count)?;
            }
            ProcessNodeKind::CallActivity { called_definition_id, called_version,
                called_element, input_mapping, output_mapping } => {
                if !called_definition_id.is_empty() {
                    uuid::Uuid::parse_str(called_definition_id)
                        .with_context(|| format!("call activity {} has invalid target definition", node.id))?;
                }
                ensure!(*called_version == 0 || !called_definition_id.is_empty(),
                    "call activity {} has a version without a target", node.id);
                if !called_element.namespace_uri.is_empty() {
                    ensure!(called_element.namespace_uri.len() <= 1024
                        && !called_element.namespace_uri.chars().any(char::is_whitespace)
                        && !called_element.namespace_uri.chars().any(char::is_control),
                        "call activity {} has invalid namespace", node.id);
                    url::Url::parse(&called_element.namespace_uri)
                        .with_context(|| format!("call activity {} has invalid namespace", node.id))?;
                }
                ensure!(called_element.process_id.is_empty() || valid_id(&called_element.process_id),
                    "call activity {} has invalid process ID", node.id);
                validate_mapping(input_mapping, false)?;
                validate_mapping(output_mapping, false)?;
            }
            _ => {}
        }
        if depth > 0 {
            ensure!(!matches!(node.kind, ProcessNodeKind::TimerStart { .. }
                | ProcessNodeKind::MessageStart { .. }),
                "timer and message starts are root-only: {}", node.id);
        }
    }
    for node in nodes {
        if let Some(attached_to_id) = match &node.kind {
            ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
            | ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
            | ProcessNodeKind::BoundaryError { attached_to_id, .. }
            | ProcessNodeKind::BoundaryEscalation { attached_to_id, .. } => Some(attached_to_id),
            _ => None,
        } {
            ensure!(!nodes.iter().any(|attached| attached.id == *attached_to_id && attached.repeat.is_some()),
                "repeat {} cannot have a boundary event", attached_to_id);
        }
    }
    let mut flow_ids = HashSet::new();
    for flow in flows {
        ensure!(valid_id(&flow.id) && flow_ids.insert(flow.id.as_str())
            && all_ids.insert(flow.id.as_str()),
            "invalid or duplicate BPMN ID: {}", flow.id);
        if let Some(expression) = &flow.condition {
            validate_expression(expression, "sequence flow condition", true)?;
        }
    }
    let node_map = nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    validate_diagram(diagram, nodes, flows, &node_map, &flow_ids)
}

pub fn validate_model(model: &ProcessModel) -> Result<()> {
    validate_draft(model)?;
    let message_ids: HashSet<_> = model
        .messages
        .iter()
        .map(|message| message.message_id.as_str())
        .collect();
    let error_ids: HashSet<_> = model
        .errors
        .iter()
        .map(|error| error.error_id.as_str())
        .collect();
    let escalation_ids: HashSet<_> = model
        .escalations
        .iter()
        .map(|escalation| escalation.escalation_id.as_str())
        .collect();
    let signal_ids: HashSet<_> = model.signals.iter().map(|signal| signal.signal_id.as_str()).collect();
    let mut used_messages = HashSet::new();
    let mut used_errors = HashSet::new();
    let mut used_escalations = HashSet::new();
    let mut used_signals = HashSet::new();
    validate_body(
        &model.nodes,
        &model.sequence_flows,
        &model.diagram,
        &model.variables,
        0,
        &message_ids,
        &error_ids,
        &escalation_ids,
        &signal_ids,
        &mut used_messages,
        &mut used_errors,
        &mut used_escalations,
        &mut used_signals,
    )?;
    ensure!(
        message_ids == used_messages,
        "unreferenced message declaration"
    );
    ensure!(error_ids == used_errors, "unreferenced error declaration");
    ensure!(
        escalation_ids == used_escalations,
        "unreferenced escalation declaration"
    );
    ensure!(signal_ids == used_signals, "unreferenced signal declaration");
    Ok(())
}

fn validate_body<'a>(
    graph_nodes: &'a [ProcessNode],
    graph_flows: &'a [ProcessSequenceFlow],
    diagram: &ProcessDiagram,
    variables: &BTreeMap<String, serde_json::Value>,
    depth: usize,
    message_ids: &HashSet<&str>,
    error_ids: &HashSet<&str>,
    escalation_ids: &HashSet<&str>,
    signal_ids: &HashSet<&str>,
    used_messages: &mut HashSet<&'a str>,
    used_errors: &mut HashSet<&'a str>,
    used_escalations: &mut HashSet<&'a str>,
    used_signals: &mut HashSet<&'a str>,
) -> Result<()> {
    ensure!(
        !graph_nodes.is_empty() && !graph_flows.is_empty(),
        "process body requires nodes and sequence flows"
    );
    let mut nodes = HashMap::new();
    let mut boundary_error_handlers = HashSet::new();
    let mut boundary_escalation_handlers = HashSet::new();
    for node in graph_nodes {
        ensure!(valid_id(&node.id), "invalid node ID: {}", node.id);
        ensure!(
            node.name.len() <= 256 && !node.name.chars().any(char::is_control),
            "invalid node name: {}",
            node.id
        );
        ensure!(
            nodes.insert(node.id.as_str(), node).is_none(),
            "duplicate node ID: {}",
            node.id
        );
        validate_repeat(node, variables, true)?;
        match &node.kind {
            ProcessNodeKind::ManualTask { assignee_user_id, instructions } => {
                ensure!(!instructions.is_empty(), "manual task {} requires instructions", node.id);
                ensure!(valid_manual_instructions(instructions),
                    "manual task {} has invalid instructions", node.id);
                ensure!(assignee_user_id.as_ref().map_or(true, |user| !user.is_empty()),
                    "manual task {} has empty assignee", node.id);
            }
            ProcessNodeKind::ScriptTask { script, output_mapping } => {
                ensure!(!script.is_empty(), "script task {} requires an expression", node.id);
                ensure!(script.len() <= 4096, "script task {} expression is too long", node.id);
                expr::validate_script_profile(script)
                    .with_context(|| format!("script task {} expression", node.id))?;
                validate_mapping(output_mapping, true)?;
            }
            ProcessNodeKind::ServiceTask {
                flow_id,
                input_mapping,
                output_mapping,
                verification,
                timeout_seconds,
                result_expression,
            } => {
                ensure!(!flow_id.is_empty(), "service task {} lacks a flow", node.id);
                ensure!(
                    (1..=600).contains(timeout_seconds),
                    "service task {} timeout is outside 1..=600 seconds",
                    node.id
                );
                validate_mapping(input_mapping, false)?;
                validate_mapping(output_mapping, false)?;
                if let tentaflow_protocol::processes::ActivityVerification::Condition {
                    expression,
                } = verification
                {
                    expr::validate_syntax(expression, None)
                        .with_context(|| format!("service task {} verification", node.id))?;
                }
                if let Some(expression) = result_expression {
                    validate_expression(expression, "service result expression", true)?;
                }
            }
            ProcessNodeKind::UserTask {
                assignee_user_id,
                output_mapping,
            } => {
                if let Some(user_id) = assignee_user_id {
                    ensure!(
                        !user_id.is_empty(),
                        "user task {} has empty assignee",
                        node.id
                    );
                }
                validate_mapping(output_mapping, false)?;
            }
            ProcessNodeKind::MessageStart { message_ref, output_mapping } => {
                ensure!(message_ids.contains(message_ref.as_str()), "message start {} references an unknown declaration", node.id);
                used_messages.insert(message_ref.as_str());
                validate_mapping(output_mapping, false)?;
            }
            ProcessNodeKind::MessageCatch { message_ref, correlation_expression, output_mapping }
            | ProcessNodeKind::ReceiveTask { message_ref, correlation_expression, output_mapping }
            | ProcessNodeKind::BoundaryMessage { message_ref, correlation_expression, output_mapping, .. } => {
                ensure!(message_ids.contains(message_ref.as_str()), "message event {} references an unknown declaration", node.id);
                used_messages.insert(message_ref.as_str());
                validate_expression(correlation_expression, "message correlation expression", true)?;
                validate_mapping(output_mapping, false)?;
            }
            ProcessNodeKind::MessageThrow { message_ref, target, correlation_expression, payload_expression, ttl_seconds }
            | ProcessNodeKind::SendTask { message_ref, target, correlation_expression, payload_expression, ttl_seconds } => {
                ensure!(message_ids.contains(message_ref.as_str()), "message throw {} references an unknown declaration", node.id);
                used_messages.insert(message_ref.as_str());
                validate_message_target(target, true)?;
                validate_expression(correlation_expression, "message correlation expression", true)?;
                validate_expression(payload_expression, "message payload expression", true)?;
                ensure!((1..=604_800).contains(ttl_seconds), "message TTL outside 1..=604800 seconds");
            }
            ProcessNodeKind::SignalCatch { signal_ref, output_mapping } => {
                ensure!(signal_ids.contains(signal_ref.as_str()),
                    "signal catch {} references an unknown declaration", node.id);
                used_signals.insert(signal_ref.as_str());
                validate_mapping(output_mapping, false)?;
            }
            ProcessNodeKind::SignalThrow { signal_ref, payload_expression, ttl_seconds } => {
                ensure!(signal_ids.contains(signal_ref.as_str()),
                    "signal throw {} references an unknown declaration", node.id);
                used_signals.insert(signal_ref.as_str());
                validate_expression(payload_expression, "signal payload expression", true)?;
                ensure!((1..=604_800).contains(ttl_seconds), "signal TTL outside 1..=604800 seconds");
            }
            ProcessNodeKind::BoundaryError { attached_to_id, error_ref, output_mapping } => {
                if let Some(reference) = error_ref {
                    ensure!(error_ids.contains(reference.as_str()), "boundary error {} references an unknown declaration", node.id);
                    used_errors.insert(reference.as_str());
                }
                ensure!(
                    boundary_error_handlers.insert((attached_to_id.as_str(), error_ref.as_deref())),
                    "duplicate boundary error handler on {}",
                    attached_to_id
                );
                validate_mapping(output_mapping, false)?;
            }
            ProcessNodeKind::BoundaryEscalation {
                attached_to_id,
                escalation_ref,
                output_mapping,
                ..
            } => {
                if let Some(reference) = escalation_ref {
                    ensure!(
                        escalation_ids.contains(reference.as_str()),
                        "boundary escalation {} references an unknown declaration",
                        node.id
                    );
                    used_escalations.insert(reference.as_str());
                }
                ensure!(
                    boundary_escalation_handlers
                        .insert((attached_to_id.as_str(), escalation_ref.as_deref())),
                    "duplicate boundary escalation handler on {}",
                    attached_to_id
                );
                validate_mapping(output_mapping, false)?;
            }
            ProcessNodeKind::SubProcess {
                body,
                input_mapping,
                output_mapping,
            } => {
                ensure!(depth < 3, "embedded subprocess depth exceeds three levels");
                validate_mapping(input_mapping, false)?;
                validate_mapping(output_mapping, false)?;
                validate_body(
                    &body.nodes,
                    &body.sequence_flows,
                    &body.diagram,
                    &body.variables,
                    depth + 1,
                    message_ids,
                    error_ids,
                    escalation_ids,
                    signal_ids,
                    used_messages,
                    used_errors,
                    used_escalations,
                    used_signals,
                )
                .with_context(|| format!("embedded subprocess {}", node.id))?;
            }
            ProcessNodeKind::CallActivity {
                called_definition_id,
                called_version,
                called_element,
                input_mapping,
                output_mapping,
            } => {
                uuid::Uuid::parse_str(called_definition_id).with_context(|| {
                    format!("call activity {} has invalid target definition", node.id)
                })?;
                ensure!(
                    *called_version > 0
                        && valid_id(&called_element.process_id)
                        && !called_element.namespace_uri.is_empty(),
                    "call activity {} requires an exact published target QName and version",
                    node.id
                );
                validate_mapping(input_mapping, false)?;
                validate_mapping(output_mapping, false)?;
            }
            ProcessNodeKind::ErrorEnd { error_ref } => {
                ensure!(error_ids.contains(error_ref.as_str()),
                    "error end {} references an unknown declaration", node.id);
                used_errors.insert(error_ref.as_str());
            }
            _ => {}
        }
    }
    let mut outgoing: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut incoming: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut flow_ids = HashSet::new();
    for flow in graph_flows {
        ensure!(valid_id(&flow.id), "invalid sequence flow ID: {}", flow.id);
        ensure!(
            flow_ids.insert(flow.id.as_str()),
            "duplicate sequence flow ID: {}",
            flow.id
        );
        ensure!(
            nodes.contains_key(flow.source_id.as_str())
                && nodes.contains_key(flow.target_id.as_str()),
            "sequence flow {} references missing node",
            flow.id
        );
        ensure!(
            flow.source_id != flow.target_id,
            "self-loop {} is unsupported in B1",
            flow.id
        );
        if let Some(condition) = &flow.condition {
            expr::validate_syntax(condition, None)
                .with_context(|| format!("sequence flow {} condition", flow.id))?;
        }
        outgoing
            .entry(&flow.source_id)
            .or_default()
            .push(&flow.target_id);
        incoming
            .entry(&flow.target_id)
            .or_default()
            .push(&flow.source_id);
    }

    let starts: Vec<_> = graph_nodes.iter()
        .filter(|node| matches!(node.kind, ProcessNodeKind::Start | ProcessNodeKind::TimerStart { .. } | ProcessNodeKind::MessageStart { .. }))
        .collect();
    if depth > 0 {
        ensure!(starts.iter().all(|node| matches!(node.kind, ProcessNodeKind::Start)),
            "timer and message starts are root-only");
    }
    ensure!(
        starts.len() == 1,
        "process requires exactly one start event"
    );
    ensure!(
        graph_nodes.iter()
        .any(|node| matches!(node.kind, ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. } | ProcessNodeKind::TerminateEnd)),
        "process requires an end event"
    );
    for node in graph_nodes {
        let in_count = incoming.get(node.id.as_str()).map_or(0, Vec::len);
        let out_count = outgoing.get(node.id.as_str()).map_or(0, Vec::len);
        match &node.kind {
            ProcessNodeKind::Start | ProcessNodeKind::TimerStart { .. } | ProcessNodeKind::MessageStart { .. } => ensure!(
                in_count == 0 && out_count == 1,
                "start event must have one outgoing flow and no incoming flow"
            ),
            ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
            | ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
            | ProcessNodeKind::BoundaryError { attached_to_id, .. }
            | ProcessNodeKind::BoundaryEscalation { attached_to_id, .. } => {
                ensure!(
                    in_count == 0 && out_count == 1,
                    "boundary event {} needs one outgoing flow and no incoming flow",
                    node.id
                );
                ensure!(
                    matches!(
                        nodes.get(attached_to_id.as_str()).map(|node| &node.kind),
                        Some(
                            ProcessNodeKind::ServiceTask { .. }
                                | ProcessNodeKind::SubProcess { .. }
                                | ProcessNodeKind::CallActivity { .. }
                        )
                    ) || (!matches!(
                        node.kind,
                        ProcessNodeKind::BoundaryError { .. }
                            | ProcessNodeKind::BoundaryEscalation { .. }
                    ) && matches!(
                        nodes.get(attached_to_id.as_str()).map(|node| &node.kind),
                        Some(ProcessNodeKind::UserTask { .. })
                    )),
                    "boundary event {} has an unsupported attachment",
                    node.id
                );
                if matches!(node.kind, ProcessNodeKind::BoundaryEscalation { .. }) {
                    ensure!(
                        matches!(nodes.get(attached_to_id.as_str()).map(|node| &node.kind),
                        Some(ProcessNodeKind::ServiceTask { result_expression: Some(expression), .. })
                            if !expression.is_empty()),
                        "boundary escalation {} requires a ServiceTask with result expression",
                        node.id
                    );
                }
            }
            ProcessNodeKind::EventBasedGateway => {
                ensure!(
                    in_count == 1 && (2..=8).contains(&out_count),
                    "event gateway {} needs one incoming and 2..=8 outgoing flows",
                    node.id
                );
                for flow in graph_flows.iter().filter(|flow| flow.source_id == node.id) {
                    ensure!(flow.condition.is_none(), "event gateway {} cannot have conditions", node.id);
                    let branch = nodes[flow.target_id.as_str()];
                    ensure!(
                        matches!(branch.kind, ProcessNodeKind::MessageCatch { .. })
                            || matches!(&branch.kind, ProcessNodeKind::TimerCatch { timer } if matches!(timer, ProcessTimerSpec::Date { .. } | ProcessTimerSpec::Duration { .. } | ProcessTimerSpec::WorkingDuration { .. })),
                        "event gateway {} must branch directly to one-shot catches",
                        node.id
                    );
                    ensure!(incoming.get(branch.id.as_str()).map_or(0, Vec::len) == 1
                        && outgoing.get(branch.id.as_str()).map_or(0, Vec::len) == 1,
                        "event gateway child {} must have one incoming and outgoing flow",
                        branch.id
                    );
                }
            }
            ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. } | ProcessNodeKind::TerminateEnd => ensure!(
                out_count == 0 && in_count >= 1,
                "end event must have incoming flow and no outgoing flow"
            ),
            ProcessNodeKind::ParallelGateway => ensure!(
                (in_count == 1 && out_count >= 2) || (in_count >= 2 && out_count == 1),
                "parallel gateway {} must be a split or join",
                node.id
            ),
            ProcessNodeKind::InclusiveGateway { default_flow_id } => {
                let split = in_count == 1 && (2..=8).contains(&out_count);
                let join = (2..=8).contains(&in_count) && out_count == 1;
                ensure!(split || join,
                    "inclusive gateway {} must be a split or join with 2..=8 branches", node.id);
                if split {
                    if let Some(default_id) = default_flow_id {
                        ensure!(graph_flows.iter().any(|flow| flow.id == *default_id
                            && flow.source_id == node.id && flow.condition.is_none()),
                            "inclusive gateway {} has invalid default flow {}", node.id, default_id);
                    }
                    for flow in graph_flows.iter().filter(|flow| flow.source_id == node.id) {
                        if default_flow_id.as_deref() == Some(flow.id.as_str()) {
                            ensure!(flow.condition.is_none(),
                                "inclusive gateway {} default flow {} cannot have a condition", node.id, flow.id);
                        } else {
                            let condition = flow.condition.as_deref().filter(|condition| !condition.trim().is_empty())
                                .with_context(|| format!("inclusive gateway {} flow {} requires a condition", node.id, flow.id))?;
                            validate_expression(condition, &format!("inclusive gateway {} flow {} condition", node.id, flow.id), true)?;
                        }
                    }
                } else {
                    ensure!(default_flow_id.is_none(),
                        "inclusive join {} cannot have a default flow", node.id);
                }
            }
            ProcessNodeKind::ExclusiveGateway { default_flow_id } => {
                ensure!(
                    in_count >= 1 && out_count >= 1,
                    "exclusive gateway {} has no path",
                    node.id
                );
                if let Some(default_id) = default_flow_id {
                    ensure!(
                        graph_flows.iter()
                            .any(|flow| flow.id == *default_id
                                && flow.source_id == node.id
                                && flow.condition.is_none()),
                        "exclusive gateway {} has invalid default flow",
                        node.id
                    );
                }
            }
            _ => ensure!(
                in_count == 1 && out_count == 1,
                "activity {} must have one incoming and outgoing flow",
                node.id
            ),
        }
        if !matches!(node.kind, ProcessNodeKind::ExclusiveGateway { .. } | ProcessNodeKind::InclusiveGateway { .. }) {
            ensure!(
                graph_flows.iter()
                    .filter(|flow| flow.source_id == node.id)
                    .all(|flow| flow.condition.is_none()),
                "conditions require an exclusive or inclusive gateway"
            );
        } else if matches!(node.kind, ProcessNodeKind::InclusiveGateway { .. }) && out_count == 1 {
            ensure!(graph_flows.iter().filter(|flow| flow.source_id == node.id)
                .all(|flow| flow.condition.is_none()),
                "inclusive join {} cannot have an outgoing condition", node.id);
        }
    }
    let pairs = gateway_pairs(graph_nodes, graph_flows)?;
    let joins: HashMap<&str, &str> = pairs
        .iter()
        .filter_map(|(split, pair)| pair.join_node_id.as_deref().map(|join| (join, split.as_str())))
        .collect();
    let mut graph_outgoing = outgoing.clone();
    let mut degree: HashMap<&str, usize> = nodes
        .keys()
        .map(|id| (*id, incoming.get(id).map_or(0, Vec::len)))
        .collect();
    for node in graph_nodes {
        if let ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
        | ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
        | ProcessNodeKind::BoundaryError { attached_to_id, .. }
        | ProcessNodeKind::BoundaryEscalation { attached_to_id, .. } = &node.kind
        {
            graph_outgoing
                .entry(attached_to_id.as_str())
                .or_default()
                .push(node.id.as_str());
            *degree.get_mut(node.id.as_str()).expect("boundary node validated") += 1;
        }
    }
    let mut queue = VecDeque::from([starts[0].id.as_str()]);
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(node_id) = queue.pop_front() {
        order.push(node_id);
        for target in graph_outgoing.get(node_id).into_iter().flatten() {
            let remaining = degree.get_mut(target).expect("edge target validated");
            *remaining -= 1;
            if *remaining == 0 {
                queue.push_back(target);
            }
        }
    }
    ensure!(
        order.len() == graph_nodes.len(),
        "process contains a cycle or unreachable node (unsupported in B1)"
    );
    let mut can_end = HashSet::new();
    for node_id in order.iter().rev() {
        if matches!(nodes[node_id].kind, ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. } | ProcessNodeKind::TerminateEnd)
            || graph_outgoing
                .get(node_id)
                .into_iter()
                .flatten()
                .any(|target| can_end.contains(target))
        {
            can_end.insert(*node_id);
        }
    }
    ensure!(
        can_end.len() == nodes.len(),
        "process contains a dead-end path"
    );
    let mut states: HashMap<&str, (String, Vec<String>)> = HashMap::new();
    states.insert(starts[0].id.as_str(), ("main".into(), Vec::new()));
    for node_id in &order {
        let (region, mut stack) = states
            .get(node_id)
            .cloned()
            .context("process node has no activation region")?;
        let node = nodes[node_id];
        if matches!(node.kind, ProcessNodeKind::ParallelGateway | ProcessNodeKind::InclusiveGateway { .. }) {
            if outgoing.get(node_id).is_some_and(|flows| flows.len() >= 2) {
                stack.push((*node_id).to_string());
            } else {
                let split = joins.get(node_id).context("gateway join lacks its paired split")?;
                ensure!(
                    stack.pop().as_deref() == Some(*split),
                    "gateway join {} has an invalid activation stack",
                    node_id
                );
            }
        }
        if matches!(node.kind, ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. }) {
            ensure!(stack.is_empty(), "end event has an open gateway activation");
        }
        for target in outgoing.get(node_id).into_iter().flatten() {
            if matches!(nodes[target].kind, ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. } | ProcessNodeKind::TerminateEnd) {
                if !matches!(nodes[target].kind, ProcessNodeKind::TerminateEnd) {
                    ensure!(stack.is_empty(), "boundary or main path ends inside a gateway fork");
                }
                states.entry(*target).or_insert_with(|| (region.clone(), stack.clone()));
            } else if let Some(existing) = states.get(target) {
                ensure!(
                    existing.0 == region && existing.1 == stack,
                    "boundary path merges with another active region at {}",
                    target
                );
            } else {
                states.insert(*target, (region.clone(), stack.clone()));
            }
        }
        for boundary in graph_nodes.iter().filter(|candidate| {
            matches!(&candidate.kind,
                ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
                | ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
                | ProcessNodeKind::BoundaryError { attached_to_id, .. }
                | ProcessNodeKind::BoundaryEscalation { attached_to_id, .. }
                if attached_to_id.as_str() == *node_id)
        }) {
            ensure!(
                stack.is_empty(),
                "boundary event {} attaches inside an active gateway fork",
                boundary.id
            );
            states.insert(boundary.id.as_str(), (boundary.id.clone(), Vec::new()));
        }
    }
    validate_event_gateway_regions(graph_nodes, graph_flows, &nodes, &outgoing, &order)?;
    validate_escalation_prefixes(graph_nodes, graph_flows, true)?;
    validate_diagram(diagram, graph_nodes, graph_flows, &nodes, &flow_ids)?;
    Ok(())
}

#[derive(Debug)]
pub(super) struct EscalationPathError {
    pub boundary_id: String,
    pub flow_id: Option<String>,
    pub node_id: Option<String>,
    pub reason: ProcessEscalationPathReason,
}

impl std::fmt::Display for EscalationPathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self.reason {
            ProcessEscalationPathReason::CrossBody => "cross_body",
            ProcessEscalationPathReason::ScopeEntry => "scope_entry",
            ProcessEscalationPathReason::CallEntry => "call_entry",
            ProcessEscalationPathReason::TerminalBeforeWait => "terminal_before_wait",
            ProcessEscalationPathReason::VariableWrite => "variable_write",
            ProcessEscalationPathReason::InvalidGateway => "invalid_gateway",
            ProcessEscalationPathReason::NoDurableWait => "no_durable_wait",
        };
        write!(
            formatter,
            "escalation boundary {} has unsupported immediate path {reason} at {}",
            self.boundary_id,
            self.flow_id
                .as_deref()
                .or(self.node_id.as_deref())
                .unwrap_or(&self.boundary_id)
        )
    }
}

impl std::error::Error for EscalationPathError {}

struct EscalationPrefixProof<'a> {
    boundary_id: &'a str,
    nodes: HashMap<&'a str, &'a ProcessNode>,
    outgoing: HashMap<&'a str, Vec<&'a ProcessSequenceFlow>>,
    pairs: HashMap<String, GatewayPair>,
    memo: HashMap<(String, Option<String>), bool>,
    active: HashSet<(String, Option<String>)>,
    complete: bool,
    inspections: usize,
}

impl EscalationPrefixProof<'_> {
    fn failure(
        &self,
        flow_id: Option<&str>,
        node_id: Option<&str>,
        reason: ProcessEscalationPathReason,
    ) -> anyhow::Error {
        EscalationPathError {
            boundary_id: self.boundary_id.to_string(),
            flow_id: flow_id.map(str::to_string),
            node_id: node_id.map(str::to_string),
            reason,
        }
        .into()
    }

    fn follow_one(&mut self, node_id: &str, stop_join: Option<&str>) -> Result<bool> {
        let outgoing = self.outgoing.get(node_id).cloned().unwrap_or_default();
        if outgoing.len() != 1 {
            if !self.complete {
                return Ok(false);
            }
            return Err(self.failure(
                None,
                Some(node_id),
                ProcessEscalationPathReason::NoDurableWait,
            ));
        }
        self.visit(&outgoing[0].target_id, stop_join, Some(&outgoing[0].id))
    }

    fn visit(
        &mut self,
        node_id: &str,
        stop_join: Option<&str>,
        incoming_flow: Option<&str>,
    ) -> Result<bool> {
        self.inspections += 1;
        if self.inspections > 2_129_920 {
            return Err(self.failure(
                incoming_flow,
                Some(node_id),
                ProcessEscalationPathReason::InvalidGateway,
            ));
        }
        if stop_join == Some(node_id) {
            return Ok(true);
        }
        let key = (node_id.to_string(), stop_join.map(str::to_string));
        if let Some(value) = self.memo.get(&key) {
            return Ok(*value);
        }
        let limit = self.nodes.len() * (self.pairs.len() + 1);
        if self.memo.len() + self.active.len() > limit || !self.active.insert(key.clone()) {
            return Err(self.failure(
                incoming_flow,
                Some(node_id),
                ProcessEscalationPathReason::InvalidGateway,
            ));
        }
        let result = (|| {
            let node = match self.nodes.get(node_id).copied() {
                Some(node) => node,
                None => {
                    return Err(self.failure(
                        incoming_flow,
                        Some(node_id),
                        ProcessEscalationPathReason::CrossBody,
                    ))
                }
            };
            match &node.kind {
                ProcessNodeKind::UserTask { .. }
                | ProcessNodeKind::ManualTask { .. }
                | ProcessNodeKind::ServiceTask { .. }
                | ProcessNodeKind::TimerCatch { .. }
                | ProcessNodeKind::MessageCatch { .. }
                | ProcessNodeKind::ReceiveTask { .. }
                | ProcessNodeKind::SignalCatch { .. }
                | ProcessNodeKind::EventBasedGateway => Ok(false),
                ProcessNodeKind::MessageThrow { .. } | ProcessNodeKind::SendTask { .. }
                | ProcessNodeKind::SignalThrow { .. }
                | ProcessNodeKind::ScriptTask { .. } => self.follow_one(node_id, stop_join),
                ProcessNodeKind::ExclusiveGateway { .. } => {
                    let outgoing = self.outgoing.get(node_id).cloned().unwrap_or_default();
                    if outgoing.is_empty() && !self.complete {
                        return Ok(false);
                    }
                    if outgoing.is_empty() {
                        return Err(self.failure(
                            incoming_flow,
                            Some(node_id),
                            ProcessEscalationPathReason::NoDurableWait,
                        ));
                    }
                    let mut may_sync = false;
                    for edge in outgoing {
                        may_sync |= self.visit(&edge.target_id, stop_join, Some(&edge.id))?;
                    }
                    Ok(may_sync)
                }
                ProcessNodeKind::ParallelGateway | ProcessNodeKind::InclusiveGateway { .. } => {
                    if let Some(pair) = self.pairs.get(node_id).cloned() {
                        let branches = self.outgoing.get(node_id).cloned().unwrap_or_default();
                        let mut arrivals = Vec::with_capacity(branches.len());
                        for edge in branches {
                            arrivals.push(self.visit(
                                &edge.target_id,
                                pair.join_node_id.as_deref(),
                                Some(&edge.id),
                            )?);
                        }
                        let may_join = match pair.kind {
                            GatewayKind::Parallel => arrivals.iter().all(|arrival| *arrival),
                            GatewayKind::Inclusive => arrivals.iter().any(|arrival| *arrival),
                        };
                        if may_join {
                            if let Some(join_id) = pair.join_node_id.as_deref() {
                                self.follow_one(join_id, stop_join)
                            } else {
                                Ok(false)
                            }
                        } else {
                            Ok(false)
                        }
                    } else if !self.complete {
                        Ok(false)
                    } else {
                        Err(self.failure(
                            incoming_flow,
                            Some(node_id),
                            ProcessEscalationPathReason::InvalidGateway,
                        ))
                    }
                }
                ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. } | ProcessNodeKind::TerminateEnd => Err(self.failure(
                    incoming_flow,
                    Some(node_id),
                    ProcessEscalationPathReason::TerminalBeforeWait,
                )),
                ProcessNodeKind::SubProcess { .. } => Err(self.failure(
                    incoming_flow,
                    Some(node_id),
                    ProcessEscalationPathReason::ScopeEntry,
                )),
                ProcessNodeKind::CallActivity { .. } => Err(self.failure(
                    incoming_flow,
                    Some(node_id),
                    ProcessEscalationPathReason::CallEntry,
                )),
                _ => Err(self.failure(
                    incoming_flow,
                    Some(node_id),
                    ProcessEscalationPathReason::NoDurableWait,
                )),
            }
        })();
        self.active.remove(&key);
        if let Ok(value) = &result {
            self.memo.insert(key, *value);
        }
        result
    }
}

fn validate_escalation_prefixes(
    nodes: &[ProcessNode],
    flows: &[ProcessSequenceFlow],
    complete: bool,
) -> Result<()> {
    if !nodes
        .iter()
        .any(|node| matches!(node.kind, ProcessNodeKind::BoundaryEscalation { .. }))
    {
        for node in nodes {
            if let ProcessNodeKind::SubProcess { body, .. } = &node.kind {
                validate_escalation_prefixes(&body.nodes, &body.sequence_flows, complete)?;
            }
        }
        return Ok(());
    }
    let pairs = match gateway_pairs(nodes, flows) {
        Ok(pairs) => pairs,
        Err(error) if !complete => {
            let _ = error;
            HashMap::new()
        }
        Err(error) => return Err(error),
    };
    for boundary in nodes
        .iter()
        .filter(|node| matches!(node.kind, ProcessNodeKind::BoundaryEscalation { .. }))
    {
        let starts: Vec<_> = flows
            .iter()
            .filter(|flow| flow.source_id == boundary.id)
            .collect();
        if starts.len() != 1 && !complete {
            continue;
        }
        ensure!(
            starts.len() == 1,
            "escalation boundary {} needs one outgoing flow",
            boundary.id
        );
        let mut proof = EscalationPrefixProof {
            boundary_id: &boundary.id,
            nodes: nodes.iter().map(|node| (node.id.as_str(), node)).collect(),
            outgoing: {
                let mut outgoing: HashMap<&str, Vec<&ProcessSequenceFlow>> = HashMap::new();
                for flow in flows {
                    outgoing.entry(&flow.source_id).or_default().push(flow);
                }
                outgoing
            },
            pairs: pairs.clone(),
            memo: HashMap::new(),
            active: HashSet::new(),
            complete,
            inspections: 0,
        };
        proof.visit(&starts[0].target_id, None, Some(&starts[0].id))?;
    }
    for node in nodes {
        if let ProcessNodeKind::SubProcess { body, .. } = &node.kind {
            validate_escalation_prefixes(&body.nodes, &body.sequence_flows, complete)?;
        }
    }
    Ok(())
}

fn validate_event_gateway_regions<'a>(
    graph_nodes: &'a [ProcessNode],
    graph_flows: &[ProcessSequenceFlow],
    nodes: &HashMap<&'a str, &'a tentaflow_protocol::processes::ProcessNode>,
    outgoing: &HashMap<&'a str, Vec<&'a str>>,
    order: &[&'a str],
) -> Result<()> {
    for gateway in graph_nodes.iter().filter(|node| matches!(node.kind, ProcessNodeKind::EventBasedGateway)) {
        let branches = outgoing.get(gateway.id.as_str()).context("event gateway lacks branches")?;
        let mut branch_reach = Vec::with_capacity(branches.len());
        for branch in branches {
            let mut reached = HashSet::new();
            let mut queue = VecDeque::from([*branch]);
            while let Some(current) = queue.pop_front() {
                if reached.insert(current) {
                    queue.extend(outgoing.get(current).into_iter().flatten().copied());
                }
            }
            branch_reach.push(reached);
        }
        let common = order.iter().copied().find(|node_id| {
            branch_reach.iter().all(|reach| reach.contains(node_id))
        });
        if let Some(common) = common {
            ensure!(
                matches!(nodes[common].kind, ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. } | ProcessNodeKind::TerminateEnd)
                    || matches!(&nodes[common].kind, ProcessNodeKind::ExclusiveGateway { default_flow_id: None }
                        if outgoing.get(common).map_or(0, Vec::len) == 1
                            && graph_flows.iter().filter(|flow| flow.source_id == common).all(|flow| flow.condition.is_none())),
                "event gateway {} branches merge at unsupported node {}",
                gateway.id,
                common
            );
        } else {
            ensure!(branch_reach.iter().any(|reach| reach.iter().any(|id|
                matches!(nodes[*id].kind, ProcessNodeKind::TerminateEnd))),
                "event gateway {} has no common exclusive merge or terminal escape", gateway.id);
        }
        let mut visited_regions = HashSet::new();
        for branch in branches {
            let mut reached = HashSet::new();
            let mut queue = VecDeque::from([*branch]);
            while let Some(current) = queue.pop_front() {
                if common == Some(current) || !reached.insert(current) {
                    continue;
                }
                if matches!(nodes[current].kind, ProcessNodeKind::TerminateEnd) { continue; }
                if matches!(nodes[current].kind, ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. }) {
                    ensure!(common.is_none(),
                        "event gateway {} branch ends before common merge {}", gateway.id, common.unwrap_or_default());
                    continue;
                }
                ensure!(!matches!(nodes[current].kind, ProcessNodeKind::EventBasedGateway),
                    "event gateway {} has a nested event race", gateway.id);
                ensure!(visited_regions.insert(current),
                    "event gateway {} branches share node {} before merge", gateway.id, current);
                queue.extend(outgoing.get(current).into_iter().flatten().copied());
            }
        }
    }
    Ok(())
}

fn validate_mapping(mapping: &std::collections::BTreeMap<String, String>, script_profile: bool) -> Result<()> {
    ensure!(
        mapping.len() <= MAX_VARIABLE_KEYS,
        "mapping exceeds 128 keys"
    );
    for (key, expression) in mapping {
        ensure!(valid_id(key), "invalid mapping key: {key}");
        if script_profile {
            expr::validate_script_profile(expression).with_context(|| format!("mapping {key}"))?;
        } else {
            expr::validate_syntax(expression, None).with_context(|| format!("mapping {key}"))?;
        }
    }
    Ok(())
}

fn validate_diagram(
    diagram: &ProcessDiagram,
    graph_nodes: &[ProcessNode],
    graph_flows: &[ProcessSequenceFlow],
    nodes: &HashMap<&str, &ProcessNode>,
    flow_ids: &HashSet<&str>,
) -> Result<()> {
    ensure!(
        diagram.shapes.len() <= graph_nodes.len()
            && diagram.edges.len() <= graph_flows.len(),
        "process diagram exceeds graph element count"
    );
    let mut shape_ids = HashSet::new();
    for shape in &diagram.shapes {
        ensure!(
            nodes.contains_key(shape.element_id.as_str())
                && shape_ids.insert(shape.element_id.as_str()),
            "diagram shape has missing or duplicate element"
        );
        ensure!(
            [shape.x, shape.y, shape.width, shape.height]
                .iter()
                .all(|value| value.is_finite() && value.abs() <= MAX_DI_COORDINATE)
                && shape.width > 0.0
                && shape.height > 0.0,
            "invalid process shape bounds"
        );
    }
    let mut edge_ids = HashSet::new();
    for edge in &diagram.edges {
        ensure!(
            flow_ids.contains(edge.sequence_flow_id.as_str())
                && edge_ids.insert(edge.sequence_flow_id.as_str()),
            "diagram edge has missing or duplicate sequence flow"
        );
        ensure!(
            edge.waypoints.len() >= 2
                && edge.waypoints.len() <= 64
                && edge.waypoints.iter().all(|point| {
                    point.x.is_finite()
                        && point.y.is_finite()
                        && point.x.abs() <= MAX_DI_COORDINATE
                        && point.y.abs() <= MAX_DI_COORDINATE
                }),
            "invalid process edge waypoints"
        );
    }
    Ok(())
}

/// Finds structured gateway regions and their exact join or terminal exits.
pub fn gateway_pairs(nodes: &[ProcessNode], flows: &[ProcessSequenceFlow]) -> Result<HashMap<String, GatewayPair>> {
    let kind = |node: &ProcessNode| match node.kind {
        ProcessNodeKind::ParallelGateway => Some(GatewayKind::Parallel),
        ProcessNodeKind::InclusiveGateway { .. } => Some(GatewayKind::Inclusive),
        _ => None,
    };
    let outgoing = |id: &str| flows.iter().filter(|flow| flow.source_id == id).collect::<Vec<_>>();
    let incoming = |id: &str| flows.iter().filter(|flow| flow.target_id == id).collect::<Vec<_>>();
    let by_id = nodes.iter().map(|node| (node.id.as_str(), node)).collect::<HashMap<_, _>>();
    let joins: Vec<_> = nodes.iter().filter(|node| kind(node).is_some() && incoming(&node.id).len() >= 2).collect();
    let splits: Vec<_> = nodes.iter().filter(|node| kind(node).is_some() && outgoing(&node.id).len() >= 2).collect();
    let trace = |split: &ProcessNode, join: Option<&ProcessNode>| {
        let mut branches = BTreeMap::new();
        let mut branch_paths = Vec::new();
        for branch in outgoing(&split.id) {
            let mut seen = HashSet::new();
            let mut arrivals = BTreeSet::new();
            let mut terminals = BTreeSet::new();
            let mut pending = vec![(branch.target_id.as_str(), branch.id.as_str())];
            while let Some((node_id, arrival_id)) = pending.pop() {
                if join.is_some_and(|candidate| candidate.id == node_id) {
                    arrivals.insert(arrival_id.to_string());
                    continue;
                }
                let node = by_id.get(node_id)?;
                if matches!(node.kind, ProcessNodeKind::TerminateEnd) {
                    terminals.insert(node_id.to_string());
                    continue;
                }
                if !seen.insert(node_id) { continue; }
                let next = outgoing(node_id);
                if next.is_empty() { return None; }
                pending.extend(next.iter().map(|edge| (edge.target_id.as_str(), edge.id.as_str())));
            }
            if arrivals.len() > 1 || arrivals.is_empty() && terminals.is_empty() { return None; }
            branch_paths.push(seen);
            branches.insert(branch.id.clone(), GatewayBranchExits {
                join_incoming_edge_id: arrivals.into_iter().next(),
                terminate_end_node_ids: terminals,
            });
        }
        if branch_paths.iter().enumerate().any(|(index, path)|
            branch_paths.iter().skip(index + 1).any(|other| !path.is_disjoint(other))) {
            return None;
        }
        if let Some(join) = join {
            let arrivals = branches.values().filter_map(|exit| exit.join_incoming_edge_id.as_ref()).collect::<Vec<_>>();
            if arrivals.len() != incoming(&join.id).len()
                || arrivals.iter().collect::<HashSet<_>>().len() != arrivals.len() {
                return None;
            }
        }
        Some(branches)
    };
    let mut pairs = HashMap::new();
    let mut used_joins = HashSet::new();
    for split in splits {
        let mut candidates = Vec::new();
        for join in &joins {
            if kind(split) == kind(join) {
                if let Some(branches) = trace(split, Some(join)) {
                    candidates.push((Some(join.id.clone()), branches));
                }
            }
        }
        if candidates.is_empty() {
            if let Some(branches) = trace(split, None) {
                candidates.push((None, branches));
            }
        }
        ensure!(candidates.len() == 1,
            "gateway split {} requires one disjoint paired join or terminal escape with unique incoming edges", split.id);
        let (join_node_id, branches) = candidates.pop().expect("one candidate checked");
        if let Some(join_id) = &join_node_id {
            ensure!(used_joins.insert(join_id.clone()), "gateway join {} is paired twice", join_id);
        }
        pairs.insert(split.id.clone(), GatewayPair {
            kind: kind(split).expect("split kind checked"),
            split_node_id: split.id.clone(), join_node_id, branches,
        });
    }
    ensure!(used_joins.len() == joins.len(), "unpaired structured gateway join");
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escalation_model() -> ProcessModel {
        use tentaflow_protocol::processes::{ActivityVerification, ProcessEscalationDeclaration};
        let mut model = starter_model();
        model.escalations.push(ProcessEscalationDeclaration {
            escalation_id: "Escalation_1".into(),
            name: "Human review".into(),
            escalation_code: "NEEDS.HUMAN".into(),
        });
        model.nodes.extend([
            ProcessNode { repeat: None,
                id: "Service_1".into(),
                name: "Check".into(),
                kind: ProcessNodeKind::ServiceTask {
                    flow_id: "flow-123".into(),
                    input_mapping: BTreeMap::new(),
                    output_mapping: BTreeMap::new(),
                    verification: ActivityVerification::Human,
                    timeout_seconds: 60,
                    result_expression: Some("outputs.result".into()),
                },
            },
            ProcessNode { repeat: None,
                id: "Boundary_1".into(),
                name: "Escalate".into(),
                kind: ProcessNodeKind::BoundaryEscalation {
                    attached_to_id: "Service_1".into(),
                    escalation_ref: Some("Escalation_1".into()),
                    cancel_activity: false,
                    output_mapping: BTreeMap::from([(
                        "review_key".into(),
                        "outputs.customer_ID".into(),
                    )]),
                },
            },
            ProcessNode { repeat: None,
                id: "Wait_1".into(),
                name: "Review".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
            },
        ]);
        model.sequence_flows[0].target_id = "Service_1".into();
        for (id, source, target) in [
            ("Flow_Normal", "Service_1", "End_1"),
            ("Flow_Escalation", "Boundary_1", "Wait_1"),
            ("Flow_Review", "Wait_1", "End_1"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                id: id.into(),
                source_id: source.into(),
                target_id: target.into(),
                condition: None,
            });
        }
        model
    }

    #[test]
    fn escalation_declarations_and_immediate_routes_require_a_real_durable_wait() {
        let mut model = escalation_model();
        validate_model(&model).unwrap();
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "Flow_Escalation")
            .unwrap()
            .target_id = "End_1".into();
        model.nodes.retain(|node| node.id != "Wait_1");
        model.sequence_flows.retain(|flow| flow.id != "Flow_Review");
        let error = validate_model(&model).unwrap_err();
        let path = error
            .downcast_ref::<EscalationPathError>()
            .expect("typed path diagnostic");
        assert_eq!(path.boundary_id, "Boundary_1");
        assert_eq!(path.flow_id.as_deref(), Some("Flow_Escalation"));
        assert_eq!(path.reason, ProcessEscalationPathReason::TerminalBeforeWait);
        model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind = ProcessNodeKind::TerminateEnd;
        assert_eq!(validate_model(&model).unwrap_err().downcast_ref::<EscalationPathError>().unwrap().reason,
            ProcessEscalationPathReason::TerminalBeforeWait);
        model = escalation_model();
        model.escalations[0].escalation_code = "invalid code".into();
        assert!(validate_draft(&model).is_err());
        model = escalation_model();
        model.escalations.push(model.escalations[0].clone());
        assert!(validate_draft(&model).is_err());
        model = escalation_model();
        if let ProcessNodeKind::ServiceTask {
            result_expression, ..
        } = &mut model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Service_1")
            .unwrap()
            .kind
        {
            *result_expression = Some(String::new());
        }
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("invalid service result expression length"));
        if let ProcessNodeKind::ServiceTask {
            result_expression, ..
        } = &mut model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Service_1")
            .unwrap()
            .kind
        {
            *result_expression = None;
        }
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("requires a ServiceTask with result expression"));
    }

    #[test]
    fn escalation_prefix_distinguishes_or_singleton_from_and_waiting_branch() {
        let mut model = escalation_model();
        model.nodes.extend([
            ProcessNode { repeat: None,
                id: "Split_1".into(),
                name: "Split".into(),
                kind: ProcessNodeKind::InclusiveGateway {
                    default_flow_id: Some("To_Sync".into()),
                },
            },
            ProcessNode { repeat: None,
                id: "Join_1".into(),
                name: "Join".into(),
                kind: ProcessNodeKind::InclusiveGateway {
                    default_flow_id: None,
                },
            },
            ProcessNode { repeat: None,
                id: "Xor_1".into(),
                name: "Immediate".into(),
                kind: ProcessNodeKind::ExclusiveGateway {
                    default_flow_id: None,
                },
            },
        ]);
        model.sequence_flows.retain(|flow| flow.id != "Flow_Review");
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "Flow_Escalation")
            .unwrap()
            .target_id = "Split_1".into();
        for (id, source, target, condition) in [
            (
                "To_Wait",
                "Split_1",
                "Wait_1",
                Some("vars.select_wait == true"),
            ),
            ("To_Sync", "Split_1", "Xor_1", None),
            ("From_Wait", "Wait_1", "Join_1", None),
            ("From_Sync", "Xor_1", "Join_1", None),
            ("After_Join", "Join_1", "End_1", None),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                id: id.into(),
                source_id: source.into(),
                target_id: target.into(),
                condition: condition.map(str::to_string),
            });
        }
        model
            .variables
            .insert("select_wait".into(), serde_json::json!(true));
        let error = validate_model(&model).unwrap_err();
        assert_eq!(
            error.downcast_ref::<EscalationPathError>().unwrap().reason,
            ProcessEscalationPathReason::TerminalBeforeWait
        );
        for node in &mut model.nodes {
            if node.id == "Split_1" || node.id == "Join_1" {
                node.kind = ProcessNodeKind::ParallelGateway;
            }
        }
        for flow in &mut model.sequence_flows {
            if flow.id == "To_Wait" {
                flow.condition = None;
            }
        }
        validate_model(&model).unwrap();
    }

    #[test]
    fn inclusive_pair_maps_selected_edges_and_rejects_invalid_conditions_and_crossing() {
        let mut model = starter_model();
        model.nodes.extend([
            ProcessNode { repeat: None, id: "Split".into(), name: "Select".into(),
                kind: ProcessNodeKind::InclusiveGateway { default_flow_id: Some("To_C".into()) } },
            ProcessNode { repeat: None, id: "Join".into(), name: "Synchronize".into(),
                kind: ProcessNodeKind::InclusiveGateway { default_flow_id: None } },
        ]);
        for id in ["A", "B", "C"] {
            model.nodes.push(ProcessNode { repeat: None, id: id.into(), name: id.into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: Default::default() } });
        }
        model.sequence_flows[0].target_id = "Split".into();
        for (id, source, target, condition) in [
            ("To_A", "Split", "A", Some("vars.a == true")),
            ("To_B", "Split", "B", Some("vars.b == true")),
            ("To_C", "Split", "C", None),
            ("From_A", "A", "Join", None),
            ("From_B", "B", "Join", None),
            ("From_C", "C", "Join", None),
            ("After_Join", "Join", "End_1", None),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                id: id.into(), source_id: source.into(), target_id: target.into(),
                condition: condition.map(str::to_string),
            });
        }
        validate_model(&model).unwrap();
        let pair = gateway_pairs(&model.nodes, &model.sequence_flows).unwrap().remove("Split").unwrap();
        assert_eq!(pair.kind, GatewayKind::Inclusive);
        assert_eq!(pair.join_node_id.as_deref(), Some("Join"));
        for (branch, arrival) in [("To_A", "From_A"), ("To_B", "From_B"), ("To_C", "From_C")] {
            assert_eq!(pair.branches[branch].join_incoming_edge_id.as_deref(), Some(arrival));
            assert!(pair.branches[branch].terminate_end_node_ids.is_empty());
        }

        model.sequence_flows.iter_mut().find(|flow| flow.id == "To_B").unwrap().condition = None;
        assert!(validate_model(&model).unwrap_err().to_string().contains("To_B"));
        model.sequence_flows.iter_mut().find(|flow| flow.id == "To_B").unwrap().condition = Some("vars.b == true".into());
        model.sequence_flows.iter_mut().find(|flow| flow.id == "To_C").unwrap().condition = Some("true".into());
        assert!(validate_model(&model).unwrap_err().to_string().contains("default flow"));
        model.sequence_flows.iter_mut().find(|flow| flow.id == "To_C").unwrap().condition = None;
        model.sequence_flows.iter_mut().find(|flow| flow.id == "From_A").unwrap().target_id = "B".into();
        assert!(validate_model(&model).is_err(), "one branch cannot enter another selected branch");
    }

    #[test]
    fn parallel_pair_keeps_nine_branches_without_the_inclusive_limit() {
        let mut model = starter_model();
        model.nodes.extend([
            ProcessNode { repeat: None, id: "Split".into(), name: String::new(), kind: ProcessNodeKind::ParallelGateway },
            ProcessNode { repeat: None, id: "Join".into(), name: String::new(), kind: ProcessNodeKind::ParallelGateway },
        ]);
        model.sequence_flows[0].target_id = "Split".into();
        for branch in 0..9 {
            let id = format!("Branch_{branch}");
            model.nodes.push(ProcessNode { repeat: None, id: id.clone(), name: id.clone(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: Default::default() } });
            model.sequence_flows.push(ProcessSequenceFlow {
                id: format!("To_{branch}"), source_id: "Split".into(), target_id: id.clone(), condition: None,
            });
            model.sequence_flows.push(ProcessSequenceFlow {
                id: format!("From_{branch}"), source_id: id, target_id: "Join".into(), condition: None,
            });
        }
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "After_Join".into(), source_id: "Join".into(), target_id: "End_1".into(), condition: None,
        });
        validate_model(&model).unwrap();
        assert_eq!(gateway_pairs(&model.nodes, &model.sequence_flows).unwrap()["Split"].branches.len(), 9);
        model.nodes.iter_mut().find(|node| node.id == "Split").unwrap().kind =
            ProcessNodeKind::InclusiveGateway { default_flow_id: None };
        model.nodes.iter_mut().find(|node| node.id == "Join").unwrap().kind =
            ProcessNodeKind::InclusiveGateway { default_flow_id: None };
        assert!(validate_model(&model).unwrap_err().to_string().contains("Split"));
    }

    #[test]
    fn terminate_end_closes_open_gateway_branches_without_inventing_a_join() {
        let mut model = starter_model();
        model.nodes.extend([
            ProcessNode { repeat: None, id: "Split".into(), name: "Parallel".into(), kind: ProcessNodeKind::ParallelGateway },
            ProcessNode { repeat: None, id: "Terminate_A".into(), name: "Stop A".into(), kind: ProcessNodeKind::TerminateEnd },
            ProcessNode { repeat: None, id: "Terminate_B".into(), name: "Stop B".into(), kind: ProcessNodeKind::TerminateEnd },
        ]);
        model.sequence_flows[0].target_id = "Split".into();
        model.sequence_flows.extend([
            ProcessSequenceFlow { id: "To_A".into(), source_id: "Split".into(), target_id: "Terminate_A".into(), condition: None },
            ProcessSequenceFlow { id: "To_B".into(), source_id: "Split".into(), target_id: "Terminate_B".into(), condition: None },
        ]);
        model.nodes.retain(|node| node.id != "End_1");
        model.diagram.shapes.retain(|shape| shape.element_id != "End_1");
        validate_model(&model).unwrap();
        let pair = &gateway_pairs(&model.nodes, &model.sequence_flows).unwrap()["Split"];
        assert_eq!(pair.join_node_id, None);
        assert_eq!(pair.branches["To_A"].terminate_end_node_ids, BTreeSet::from(["Terminate_A".into()]));
        assert_eq!(pair.branches["To_B"].terminate_end_node_ids, BTreeSet::from(["Terminate_B".into()]));
        assert!(pair.branches.values().all(|branch| branch.join_incoming_edge_id.is_none()));

        model.nodes.iter_mut().find(|node| node.id == "Terminate_B").unwrap().kind = ProcessNodeKind::End;
        assert!(validate_model(&model).is_err(), "ordinary End cannot close an active fork");
    }

    #[test]
    fn mixed_terminal_and_join_branches_keep_exact_join_capability() {
        let mut model = starter_model();
        model.nodes.extend([
            ProcessNode { repeat: None, id: "Split".into(), name: "Select".into(), kind: ProcessNodeKind::InclusiveGateway { default_flow_id: Some("To_Stop".into()) } },
            ProcessNode { repeat: None, id: "Join".into(), name: "Join".into(), kind: ProcessNodeKind::InclusiveGateway { default_flow_id: None } },
            ProcessNode { repeat: None, id: "Wait_A".into(), name: "A".into(), kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } },
            ProcessNode { repeat: None, id: "Wait_B".into(), name: "B".into(), kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } },
            ProcessNode { repeat: None, id: "Stop".into(), name: "Stop".into(), kind: ProcessNodeKind::TerminateEnd },
        ]);
        model.sequence_flows[0].target_id = "Split".into();
        for (id, source, target, condition) in [
            ("To_A", "Split", "Wait_A", Some("true")),
            ("To_B", "Split", "Wait_B", Some("true")),
            ("To_Stop", "Split", "Stop", None),
            ("From_A", "Wait_A", "Join", None),
            ("From_B", "Wait_B", "Join", None),
            ("From_Join", "Join", "End_1", None),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow { id: id.into(), source_id: source.into(), target_id: target.into(), condition: condition.map(str::to_string) });
        }
        validate_model(&model).unwrap();
        let pair = &gateway_pairs(&model.nodes, &model.sequence_flows).unwrap()["Split"];
        assert_eq!(pair.join_node_id.as_deref(), Some("Join"));
        assert_eq!(pair.branches["To_A"].join_incoming_edge_id.as_deref(), Some("From_A"));
        assert_eq!(pair.branches["To_B"].join_incoming_edge_id.as_deref(), Some("From_B"));
        assert_eq!(pair.branches["To_Stop"].terminate_end_node_ids, BTreeSet::from(["Stop".into()]));
        assert_eq!(pair.branches["To_Stop"].join_incoming_edge_id, None);
    }

    #[test]
    fn nested_mixed_gateways_keep_lifo_joins_and_shared_terminal_frontiers() {
        let mut model = starter_model();
        model.nodes.extend([
            ProcessNode { repeat: None, id: "OuterSplit".into(), name: "Outer OR".into(),
                kind: ProcessNodeKind::InclusiveGateway { default_flow_id: Some("Outer_Stop".into()) } },
            ProcessNode { repeat: None, id: "OuterLeft".into(), name: "Outer left".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } },
            ProcessNode { repeat: None, id: "InnerSplit".into(), name: "Inner AND".into(), kind: ProcessNodeKind::ParallelGateway },
            ProcessNode { repeat: None, id: "InnerLeft".into(), name: "Inner left".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } },
            ProcessNode { repeat: None, id: "InnerRight".into(), name: "Inner right".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } },
            ProcessNode { repeat: None, id: "InnerJoin".into(), name: "Inner join".into(), kind: ProcessNodeKind::ParallelGateway },
            ProcessNode { repeat: None, id: "OuterJoin".into(), name: "Outer join".into(),
                kind: ProcessNodeKind::InclusiveGateway { default_flow_id: None } },
            ProcessNode { repeat: None, id: "SharedStop".into(), name: "Shared terminal".into(), kind: ProcessNodeKind::TerminateEnd },
        ]);
        model.sequence_flows[0].target_id = "OuterSplit".into();
        for (id, source, target, condition) in [
            ("Outer_Left", "OuterSplit", "OuterLeft", Some("true")),
            ("Outer_Inner", "OuterSplit", "InnerSplit", Some("true")),
            ("Outer_Stop", "OuterSplit", "SharedStop", None),
            ("OuterLeft_Join", "OuterLeft", "OuterJoin", None),
            ("Inner_Left", "InnerSplit", "InnerLeft", None),
            ("Inner_Right", "InnerSplit", "InnerRight", None),
            ("Inner_Stop", "InnerSplit", "SharedStop", None),
            ("InnerLeft_Join", "InnerLeft", "InnerJoin", None),
            ("InnerRight_Join", "InnerRight", "InnerJoin", None),
            ("Inner_Outer", "InnerJoin", "OuterJoin", None),
            ("Outer_End", "OuterJoin", "End_1", None),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                id: id.into(), source_id: source.into(), target_id: target.into(),
                condition: condition.map(str::to_string),
            });
        }
        validate_model(&model).unwrap();
        let pairs = gateway_pairs(&model.nodes, &model.sequence_flows).unwrap();
        let outer = &pairs["OuterSplit"];
        assert_eq!(outer.join_node_id.as_deref(), Some("OuterJoin"));
        assert_eq!(outer.branches["Outer_Left"].join_incoming_edge_id.as_deref(), Some("OuterLeft_Join"));
        assert_eq!(outer.branches["Outer_Inner"].join_incoming_edge_id.as_deref(), Some("Inner_Outer"));
        assert_eq!(outer.branches["Outer_Inner"].terminate_end_node_ids, BTreeSet::from(["SharedStop".into()]));
        assert_eq!(outer.branches["Outer_Stop"].join_incoming_edge_id, None);
        assert_eq!(outer.branches["Outer_Stop"].terminate_end_node_ids, BTreeSet::from(["SharedStop".into()]));
        let inner = &pairs["InnerSplit"];
        assert_eq!(inner.join_node_id.as_deref(), Some("InnerJoin"));
        assert_eq!(inner.branches["Inner_Stop"].terminate_end_node_ids, BTreeSet::from(["SharedStop".into()]));

        let mut shared_work = model.clone();
        shared_work.nodes.push(ProcessNode { repeat: None, id: "SharedWork".into(), name: "Invalid shared work".into(),
            kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } });
        for edge in &mut shared_work.sequence_flows {
            if edge.id == "Outer_Stop" || edge.id == "Inner_Stop" { edge.target_id = "SharedWork".into(); }
        }
        shared_work.sequence_flows.push(ProcessSequenceFlow {
            id: "Shared_Stop".into(), source_id: "SharedWork".into(),
            target_id: "SharedStop".into(), condition: None,
        });
        assert!(validate_model(&shared_work).is_err(), "branches may share the terminal, not prior work");

        let mut one_input_join = model.clone();
        one_input_join.sequence_flows.iter_mut().find(|edge| edge.id == "OuterLeft_Join").unwrap().target_id = "SharedStop".into();
        assert!(validate_model(&one_input_join).is_err(), "a one-input gateway cannot silently act as a join");

        let mut orphan_inner_join = model;
        orphan_inner_join.sequence_flows.iter_mut().find(|edge| edge.id == "InnerRight_Join").unwrap().target_id = "SharedStop".into();
        assert!(validate_model(&orphan_inner_join).is_err(), "the inner split needs its full paired join");
    }

    #[test]
    fn event_race_accepts_disjoint_terminate_frontiers_after_one_shot_catches() {
        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model.nodes.extend([
            ProcessNode { repeat: None, id: "Race".into(), name: "First event".into(), kind: ProcessNodeKind::EventBasedGateway },
            ProcessNode { repeat: None, id: "Timer_A".into(), name: "First".into(), kind: ProcessNodeKind::TimerCatch { timer: ProcessTimerSpec::Duration { seconds: 1 } } },
            ProcessNode { repeat: None, id: "Timer_B".into(), name: "Second".into(), kind: ProcessNodeKind::TimerCatch { timer: ProcessTimerSpec::Duration { seconds: 2 } } },
            ProcessNode { repeat: None, id: "Stop_A".into(), name: "Stop first".into(), kind: ProcessNodeKind::TerminateEnd },
            ProcessNode { repeat: None, id: "Stop_B".into(), name: "Stop second".into(), kind: ProcessNodeKind::TerminateEnd },
        ]);
        model.sequence_flows[0].target_id = "Race".into();
        for (id, source, target) in [
            ("To_A", "Race", "Timer_A"), ("To_B", "Race", "Timer_B"),
            ("From_A", "Timer_A", "Stop_A"), ("From_B", "Timer_B", "Stop_B"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow { id: id.into(), source_id: source.into(), target_id: target.into(), condition: None });
        }
        model.nodes.retain(|node| node.id != "End_1");
        model.diagram.shapes.retain(|shape| shape.element_id != "End_1");
        validate_model(&model).unwrap();
    }

    #[test]
    fn call_activity_and_error_end_require_exact_binding_and_closed_parallel_path() {
        use tentaflow_protocol::processes::{
            ProcessCallableReference, ProcessErrorDeclaration, ProcessNode, ProcessSequenceFlow,
        };

        let mut model = starter_model();
        model.errors.push(ProcessErrorDeclaration {
            error_id: "Error_Business".into(), name: "Rejected".into(),
            error_code: "BUSINESS.REJECTED".into(),
        });
        model.nodes.insert(1, ProcessNode { repeat: None,
            id: "Call_1".into(), name: "Review".into(),
            kind: ProcessNodeKind::CallActivity {
                called_definition_id: "91764f75-dadb-41aa-a252-a8a911fe7a94".into(),
                called_version: 2,
                called_element: ProcessCallableReference {
                    namespace_uri: "urn:example:review".into(),
                    process_id: "Review_Process".into(),
                },
                input_mapping: Default::default(), output_mapping: Default::default(),
            },
        });
        model.nodes[2].kind = ProcessNodeKind::ErrorEnd { error_ref: "Error_Business".into() };
        model.sequence_flows[0].target_id = "Call_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_2".into(), source_id: "Call_1".into(), target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&model).unwrap();

        if let ProcessNodeKind::CallActivity { called_version, .. } = &mut model.nodes[1].kind {
            *called_version = 0;
        }
        assert!(validate_model(&model).is_err(), "a call cannot choose latest at execution time");
        if let ProcessNodeKind::CallActivity { called_version, .. } = &mut model.nodes[1].kind {
            *called_version = 2;
        }
        model.nodes[2].kind = ProcessNodeKind::ErrorEnd { error_ref: "Missing".into() };
        assert!(validate_model(&model).is_err(), "an error end requires a declared business error");
        model.nodes[2].kind = ProcessNodeKind::ErrorEnd { error_ref: "Error_Business".into() };

        model.nodes.extend([
            ProcessNode { repeat: None, id: "Split_1".into(), name: "Split".into(), kind: ProcessNodeKind::ParallelGateway },
            ProcessNode { repeat: None, id: "Join_1".into(), name: "Join".into(), kind: ProcessNodeKind::ParallelGateway },
            ProcessNode { repeat: None, id: "Branch_A".into(), name: "A".into(), kind: ProcessNodeKind::UserTask {
                assignee_user_id: None, output_mapping: Default::default(),
            } },
            ProcessNode { repeat: None, id: "Branch_B".into(), name: "B".into(), kind: ProcessNodeKind::UserTask {
                assignee_user_id: None, output_mapping: Default::default(),
            } },
        ]);
        model.sequence_flows[0].target_id = "Split_1".into();
        for (id, source_id, target_id) in [
            ("Flow_3", "Split_1", "Branch_A"), ("Flow_4", "Split_1", "Branch_B"),
            ("Flow_5", "Branch_A", "Join_1"), ("Flow_6", "Branch_B", "Join_1"),
            ("Flow_7", "Join_1", "Call_1"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                id: id.into(), source_id: source_id.into(), target_id: target_id.into(), condition: None,
            });
        }
        validate_model(&model).unwrap();
        model.sequence_flows.iter_mut().find(|flow| flow.id == "Flow_5").unwrap().target_id = "End_1".into();
        assert!(validate_model(&model).is_err(), "an error end cannot silently consume an open parallel activation");
    }

    #[test]
    fn message_declarations_keep_incomplete_drafts_but_publication_requires_real_references() {
        use tentaflow_protocol::processes::{ProcessErrorDeclaration, ProcessMessageDeclaration};
        let mut model = starter_model();
        model.target_namespace = Some("urn:example:customer:v1".into());
        model.messages.push(ProcessMessageDeclaration { message_id: "Message_Order".into(), name: "order.received".into() });
        validate_draft(&model).unwrap();
        assert!(validate_model(&model).is_err(), "unused executable declaration cannot publish");
        model.nodes[0].kind = ProcessNodeKind::MessageStart { message_ref: "Message_Order".into(), output_mapping: Default::default() };
        validate_model(&model).unwrap();
        model.nodes[0].kind = ProcessNodeKind::MessageStart { message_ref: "Missing".into(), output_mapping: Default::default() };
        validate_draft(&model).unwrap();
        assert!(validate_model(&model).is_err(), "a draft may retain a deleted reference but publication cannot");
        model.nodes[0].kind = ProcessNodeKind::MessageStart { message_ref: "Message_Order".into(), output_mapping: Default::default() };
        model.errors.push(ProcessErrorDeclaration { error_id: "Message_Order".into(), name: "bad".into(), error_code: "BUSINESS".into() });
        assert!(validate_draft(&model).is_err(), "XML IDs are unique across declarations and nodes");
        model.errors[0].error_id = "Error_Business".into();
        assert!(validate_model(&model).is_err(), "an unused error declaration cannot publish");
    }

    #[test]
    fn event_gateway_accepts_disjoint_one_shot_branches_and_rejects_shared_activity() {
        use tentaflow_protocol::processes::{ProcessMessageDeclaration, ProcessNode, ProcessSequenceFlow};
        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model.messages.push(ProcessMessageDeclaration { message_id: "Message_1".into(), name: "signal".into() });
        model.nodes.extend([
            ProcessNode { repeat: None, id: "Race_1".into(), name: "First event".into(), kind: ProcessNodeKind::EventBasedGateway },
            ProcessNode { repeat: None, id: "Catch_Message".into(), name: "Message".into(), kind: ProcessNodeKind::MessageCatch {
                message_ref: "Message_1".into(), correlation_expression: "vars.case_id".into(), output_mapping: Default::default(),
            } },
            ProcessNode { repeat: None, id: "Catch_Timer".into(), name: "Timeout".into(), kind: ProcessNodeKind::TimerCatch {
                timer: ProcessTimerSpec::Duration { seconds: 60 },
            } },
        ]);
        model.sequence_flows[0].target_id = "Race_1".into();
        for (id, source, target) in [
            ("Flow_2", "Race_1", "Catch_Message"), ("Flow_3", "Race_1", "Catch_Timer"),
            ("Flow_4", "Catch_Message", "End_1"), ("Flow_5", "Catch_Timer", "End_1"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow { id: id.into(), source_id: source.into(), target_id: target.into(), condition: None });
        }
        validate_model(&model).unwrap();
        model.sequence_flows[4].target_id = "Catch_Message".into();
        assert!(validate_model(&model).is_err(), "a race child cannot be shared by both alternatives");
    }

    #[test]
    fn calendar_without_timer_requires_zone_and_working_rule_requires_calendar() {
        use tentaflow_protocol::processes::{HolidayPolicy, ProcessWorkCalendar, WorkWindow};
        let mut model = starter_model();
        model.work_calendar = Some(ProcessWorkCalendar {
            name: "Office".into(),
            weekly_windows: vec![WorkWindow { weekday: 1, start_minute: 540, end_minute: 1020 }],
            manual_days_off: Vec::new(), holiday_policy: HolidayPolicy::None,
        });
        assert!(validate_model(&model).is_err());
        model.timer_timezone = Some("Europe/Warsaw".into());
        validate_model(&model).unwrap();
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::WorkingDuration { seconds: 3600 },
        };
        validate_model(&model).unwrap();
        model.work_calendar = None;
        assert!(validate_model(&model).is_err());
        model.work_calendar = Some(ProcessWorkCalendar {
            name: "Office".into(), weekly_windows: Vec::new(),
            manual_days_off: Vec::new(), holiday_policy: HolidayPolicy::None,
        });
        assert!(validate_model(&model).is_err());
    }

    fn boundary_model() -> ProcessModel {
        use tentaflow_protocol::processes::{ProcessNode, ProcessSequenceFlow};

        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model.nodes.push(ProcessNode { repeat: None,
            id: "Review_1".into(),
            name: "Review".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: Default::default(),
            },
        });
        model.nodes.push(ProcessNode { repeat: None,
            id: "Boundary_A".into(),
            name: "Time limit".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Review_1".into(),
                cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 90 },
            },
        });
        model.nodes.push(ProcessNode { repeat: None,
            id: "Boundary_B".into(),
            name: "Reminder".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Review_1".into(),
                cancel_activity: false,
                timer: ProcessTimerSpec::Date {
                    at: "2027-01-02T03:04:05Z".into(),
                },
            },
        });
        model.sequence_flows[0].target_id = "Review_1".into();
        for (id, source_id) in [
            ("Flow_2", "Review_1"),
            ("Flow_3", "Boundary_A"),
            ("Flow_4", "Boundary_B"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                id: id.into(),
                source_id: source_id.into(),
                target_id: "End_1".into(),
                condition: None,
            });
        }
        model
    }

    #[test]
    fn boundary_siblings_are_real_implicit_branches_without_merging_activities() {
        use tentaflow_protocol::processes::{ProcessNode, ProcessSequenceFlow};

        let mut model = boundary_model();
        validate_model(&model).unwrap();
        let boundary = model.nodes.iter_mut().find(|node| node.id == "Boundary_A").unwrap();
        if let ProcessNodeKind::BoundaryTimer { attached_to_id, .. } = &mut boundary.kind {
            *attached_to_id = "End_1".into();
        }
        assert!(validate_model(&model).is_err());
        model = boundary_model();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_5".into(), source_id: "Review_1".into(),
            target_id: "Boundary_A".into(), condition: None,
        });
        assert!(validate_model(&model).is_err());
        model = boundary_model();
        model.nodes.push(ProcessNode { repeat: None,
            id: "Shared_1".into(), name: "Shared".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None, output_mapping: Default::default(),
            },
        });
        model.sequence_flows.iter_mut().find(|flow| flow.id == "Flow_2").unwrap().target_id = "Shared_1".into();
        model.sequence_flows.iter_mut().find(|flow| flow.id == "Flow_3").unwrap().target_id = "Shared_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_5".into(), source_id: "Shared_1".into(),
            target_id: "End_1".into(), condition: None,
        });
        assert!(validate_model(&model).is_err());
    }

    #[test]
    fn boundary_after_paired_join_and_own_closed_parallel_region_are_supported() {
        use tentaflow_protocol::processes::{ProcessNode, ProcessSequenceFlow};

        let mut model = boundary_model();
        for id in ["Split_1", "Join_1", "SideSplit_1", "SideJoin_1"] {
            model.nodes.push(ProcessNode { repeat: None,
                id: id.into(), name: id.into(), kind: ProcessNodeKind::ParallelGateway,
            });
        }
        for id in ["Branch_A", "Branch_B", "Side_A", "Side_B"] {
            model.nodes.push(ProcessNode { repeat: None,
                id: id.into(), name: id.into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None, output_mapping: Default::default(),
                },
            });
        }
        model.sequence_flows.iter_mut().find(|flow| flow.id == "Flow_1").unwrap().target_id = "Split_1".into();
        model.sequence_flows.iter_mut().find(|flow| flow.id == "Flow_3").unwrap().target_id = "SideSplit_1".into();
        for (id, source, target) in [
            ("F_A", "Split_1", "Branch_A"), ("F_B", "Split_1", "Branch_B"),
            ("F_C", "Branch_A", "Join_1"), ("F_D", "Branch_B", "Join_1"),
            ("F_E", "Join_1", "Review_1"),
            ("F_F", "SideSplit_1", "Side_A"), ("F_G", "SideSplit_1", "Side_B"),
            ("F_H", "Side_A", "SideJoin_1"), ("F_I", "Side_B", "SideJoin_1"),
            ("F_J", "SideJoin_1", "End_1"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                id: id.into(), source_id: source.into(), target_id: target.into(), condition: None,
            });
        }
        validate_model(&model).unwrap();
        let boundary = model.nodes.iter_mut().find(|node| node.id == "Boundary_A").unwrap();
        if let ProcessNodeKind::BoundaryTimer { attached_to_id, .. } = &mut boundary.kind {
            *attached_to_id = "Branch_A".into();
        }
        assert!(validate_model(&model).is_err());
    }

    #[test]
    fn timer_profiles_require_explicit_zone_and_supported_start_catch_rules() {
        let mut model = starter_model();
        model.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Cycle { seconds: 300, total_firings: Some(3) } };
        assert!(validate_model(&model).is_err());
        model.timer_timezone = Some("Europe/Warsaw".into());
        validate_model(&model).unwrap();
        model.timer_timezone = Some("Mars/Olympus".into());
        assert!(validate_model(&model).is_err());
        model.timer_timezone = Some("UTC".into());
        model.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Cycle { seconds: 299, total_firings: Some(3) } };
        assert!(validate_model(&model).is_err());
        model.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Daily { hour: 24, minute: 0, total_firings: None } };
        assert!(validate_model(&model).is_err());
        model.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Date { at: "2027-01-02T03:04:05.1234Z".into() } };
        assert!(validate_model(&model).is_err());
        model.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Date { at: "2027-01-02T03:04:05.123Z".into() } };
        validate_model(&model).unwrap();
        model.nodes.insert(1, tentaflow_protocol::processes::ProcessNode { repeat: None, id: "Wait_1".into(), name: "Wait".into(), kind: ProcessNodeKind::TimerCatch { timer: ProcessTimerSpec::Cycle { seconds: 300, total_firings: None } } });
        model.sequence_flows[0].target_id = "Wait_1".into();
        model.sequence_flows.push(tentaflow_protocol::processes::ProcessSequenceFlow { id: "Flow_2".into(), source_id: "Wait_1".into(), target_id: "End_1".into(), condition: None });
        assert!(validate_model(&model).is_err());
        model.nodes[1].kind = ProcessNodeKind::TimerCatch { timer: ProcessTimerSpec::Duration { seconds: 90 } };
        validate_model(&model).unwrap();
    }

    #[test]
    fn starter_publishes_and_cycle_is_rejected() {
        let model = starter_model();
        validate_model(&model).unwrap();
        let mut cyclic = model;
        cyclic
            .sequence_flows
            .push(tentaflow_protocol::processes::ProcessSequenceFlow {
                id: "Flow_2".into(),
                source_id: "End_1".into(),
                target_id: "Start_1".into(),
                condition: None,
            });
        assert!(validate_model(&cyclic).is_err());
    }

    #[test]
    fn diagram_rejects_huge_finite_bounds_and_waypoints() {
        use tentaflow_protocol::processes::{ProcessEdgeDiagram, ProcessPoint, ProcessShape};

        let mut model = starter_model();
        model.diagram.shapes.push(ProcessShape {
            element_id: "Start_1".into(),
            x: 25.0,
            y: 30.0,
            width: 40.0,
            height: 35.0,
        });
        model.diagram.edges.push(ProcessEdgeDiagram {
            sequence_flow_id: "Flow_1".into(),
            waypoints: vec![
                ProcessPoint { x: 65.0, y: 47.0 },
                ProcessPoint { x: 100.0, y: 47.0 },
            ],
        });
        validate_model(&model).unwrap();
        model.diagram.shapes[0].x = 1e300;
        assert!(validate_model(&model).is_err());
        model.diagram.shapes[0].x = 25.0;
        model.diagram.edges[0].waypoints[1].y = -1e300;
        assert!(validate_model(&model).is_err());
    }

    fn embedded_model() -> ProcessModel {
        use tentaflow_protocol::processes::ProcessSubProcess;
        let mut model = starter_model();
        model.nodes.insert(1, ProcessNode { repeat: None,
            id: "Sub_1".into(), name: "Review scope".into(),
            kind: ProcessNodeKind::SubProcess {
                body: ProcessSubProcess {
                    nodes: vec![
                        ProcessNode { repeat: None, id: "LocalStart".into(), name: "Start".into(), kind: ProcessNodeKind::Start },
                        ProcessNode { repeat: None, id: "LocalTask".into(), name: "Approve".into(),
                            kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } },
                        ProcessNode { repeat: None, id: "LocalEnd".into(), name: "End".into(), kind: ProcessNodeKind::End },
                    ],
                    sequence_flows: vec![
                        ProcessSequenceFlow { id: "LocalFlow1".into(), source_id: "LocalStart".into(), target_id: "LocalTask".into(), condition: None },
                        ProcessSequenceFlow { id: "LocalFlow2".into(), source_id: "LocalTask".into(), target_id: "LocalEnd".into(), condition: None },
                    ],
                    variables: BTreeMap::from([("local_ID".into(), serde_json::json!("value"))]),
                    diagram: ProcessDiagram::default(),
                },
                input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
            },
        });
        model.sequence_flows[0].target_id = "Sub_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_2".into(), source_id: "Sub_1".into(), target_id: "End_1".into(), condition: None,
        });
        model
    }

    #[test]
    fn embedded_terminate_end_is_local_to_its_own_body() {
        let mut model = embedded_model();
        if let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes[1].kind {
            body.nodes[2].kind = ProcessNodeKind::TerminateEnd;
        }
        validate_model(&model).unwrap();
        assert!(matches!(scope_body(&model, &["Sub_1".into()]).unwrap().0[2].kind,
            ProcessNodeKind::TerminateEnd));
        assert!(matches!(model.nodes[2].kind, ProcessNodeKind::End));
    }

    #[test]
    fn embedded_scope_has_local_graph_and_global_identity() {
        let mut model = embedded_model();
        validate_model(&model).unwrap();
        assert_eq!(scope_body(&model, &["Sub_1".into()]).unwrap().0.len(), 3);
        assert_eq!(all_nodes(&model).len(), 6);
        assert!(scope_body(&model, &["LocalTask".into()]).is_err());

        if let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes[1].kind {
            body.nodes[1].id = "End_1".into();
        }
        assert!(validate_model(&model).is_err());
        let mut model = embedded_model();
        if let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes[1].kind {
            body.sequence_flows[0].target_id = "End_1".into();
        }
        assert!(validate_model(&model).is_err());
        let mut model = embedded_model();
        if let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes[1].kind {
            body.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Duration { seconds: 60 } };
        }
        model.timer_timezone = Some("UTC".into());
        assert!(validate_model(&model).is_err());
    }

    #[test]
    fn repeated_tasks_require_bounded_input_and_a_declared_local_output() {
        use tentaflow_protocol::processes::{ActivityVerification, ProcessMultiInstanceMode};
        let mut model = starter_model();
        model.variables.insert("results".into(), serde_json::json!([]));
        model.variables.insert("items".into(), serde_json::json!([{"id": 1}]));
        model.nodes.insert(1, ProcessNode {
            id: "Review_1".into(), name: "Review".into(),
            kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() },
            repeat: Some(ProcessRepeatSpec::MultiInstance {
                mode: ProcessMultiInstanceMode::Parallel,
                input: ProcessMultiInstanceInput::Cardinality { count: 16 },
                output_collection_variable: "results".into(),
            }),
        });
        model.sequence_flows[0].target_id = "Review_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_2".into(), source_id: "Review_1".into(), target_id: "End_1".into(), condition: None });
        validate_model(&model).unwrap();
        model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance { mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 0 }, output_collection_variable: "results".into() });
        validate_model(&model).unwrap();
        model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance { mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::CollectionExpression { expression: "vars.items".into() }, output_collection_variable: "results".into() });
        validate_model(&model).unwrap();
        model.nodes[1].repeat = Some(ProcessRepeatSpec::StructuredLoop { condition: "vars.keep_going".into(), test_before: true,
            max_iterations: 32, output_collection_variable: "results".into() });
        validate_model(&model).unwrap();
        let old = model.nodes[1].repeat.clone();
        for invalid in [ProcessRepeatSpec::MultiInstance { mode: ProcessMultiInstanceMode::Parallel,
                input: ProcessMultiInstanceInput::Cardinality { count: 17 }, output_collection_variable: "results".into() },
            ProcessRepeatSpec::StructuredLoop { condition: "vars.keep_going".into(), test_before: true,
                max_iterations: 0, output_collection_variable: "results".into() },
            ProcessRepeatSpec::StructuredLoop { condition: "vars.keep_going".into(), test_before: true,
                max_iterations: 33, output_collection_variable: "results".into() },
            ProcessRepeatSpec::StructuredLoop { condition: "vars.keep_going".into(), test_before: true,
                max_iterations: 2, output_collection_variable: "missing".into() }] {
            model.nodes[1].repeat = Some(invalid);
            assert!(validate_model(&model).is_err());
        }
        model.nodes[1].repeat = old;
        if let ProcessNodeKind::UserTask { output_mapping, .. } = &mut model.nodes[1].kind {
            output_mapping.insert("results".into(), "outputs.value".into());
        }
        assert!(validate_model(&model).is_err());
        model.nodes[1].kind = ProcessNodeKind::ParallelGateway;
        assert!(validate_model(&model).is_err());
        model.nodes[1].kind = ProcessNodeKind::UserTask {
            assignee_user_id: None, output_mapping: BTreeMap::new(),
        };
        model.timer_timezone = Some("UTC".into());
        model.nodes.push(ProcessNode {
            id: "ReviewDeadline".into(), name: "Deadline".into(), repeat: None,
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Review_1".into(), cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 60 },
            },
        });
        assert!(validate_draft(&model).unwrap_err().to_string().contains("cannot have a boundary event"));

        let mut service = starter_model();
        service.variables.insert("results".into(), serde_json::json!([]));
        service.nodes.insert(1, ProcessNode {
            id: "ServiceReview".into(), name: "Review".into(),
            kind: ProcessNodeKind::ServiceTask {
                flow_id: "flow-review".into(), input_mapping: BTreeMap::new(),
                output_mapping: BTreeMap::new(), verification: ActivityVerification::Human,
                timeout_seconds: 60, result_expression: None,
            },
            repeat: Some(ProcessRepeatSpec::MultiInstance {
                mode: ProcessMultiInstanceMode::Sequential,
                input: ProcessMultiInstanceInput::Cardinality { count: 1 },
                output_collection_variable: "results".into(),
            }),
        });
        service.sequence_flows[0].target_id = "ServiceReview".into();
        service.sequence_flows.push(ProcessSequenceFlow {
            id: "ServiceExit".into(), source_id: "ServiceReview".into(),
            target_id: "End_1".into(), condition: None,
        });
        validate_model(&service).unwrap();

        let mut embedded = embedded_model();
        embedded.variables.insert("root_only".into(), serde_json::json!([]));
        if let ProcessNodeKind::SubProcess { body, .. } = &mut embedded.nodes[1].kind {
            body.variables.insert("local_results".into(), serde_json::json!([]));
            body.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
                mode: ProcessMultiInstanceMode::Parallel,
                input: ProcessMultiInstanceInput::Cardinality { count: 2 },
                output_collection_variable: "local_results".into(),
            });
        }
        validate_model(&embedded).unwrap();
        if let ProcessNodeKind::SubProcess { body, .. } = &mut embedded.nodes[1].kind {
            body.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
                mode: ProcessMultiInstanceMode::Parallel,
                input: ProcessMultiInstanceInput::Cardinality { count: 2 },
                output_collection_variable: "root_only".into(),
            });
        }
        let error = validate_model(&embedded).unwrap_err();
        assert!(error.to_string().contains("embedded subprocess"));
        assert!(format!("{error:#}").contains("output collection variable is undeclared"));
    }

    #[test]
    fn script_task_draft_publish_and_mapping_use_the_joined_script_profile() {
        let mut model = starter_model();
        model.variables.insert("amount".into(), serde_json::json!(2));
        model.variables.insert("answer".into(), serde_json::Value::Null);
        model.nodes.insert(1, ProcessNode {
            id: "Script_1".into(), name: "Calculate".into(), repeat: None,
            kind: ProcessNodeKind::ScriptTask { script: String::new(), output_mapping: BTreeMap::new() },
        });
        model.sequence_flows[0].target_id = "Script_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_2".into(),
            source_id: "Script_1".into(), target_id: "End_1".into(), condition: None });
        validate_draft(&model).unwrap();
        assert!(validate_model(&model).unwrap_err().to_string().contains("requires an expression"));
        let ProcessNodeKind::ScriptTask { script, output_mapping } = &mut model.nodes[1].kind else { unreachable!() };
        *script = "vars.amount + 1".into();
        output_mapping.insert("answer".into(), "outputs".into());
        validate_model(&model).unwrap();
        let ProcessNodeKind::ScriptTask { script, .. } = &mut model.nodes[1].kind else { unreachable!() };
        *script = "[".into();
        assert!(validate_draft(&model).is_err(), "a provided invalid body cannot be saved");
        let ProcessNodeKind::ScriptTask { script, .. } = &mut model.nodes[1].kind else { unreachable!() };
        *script = "vars.amount + 1".into();
        let ProcessNodeKind::ScriptTask { output_mapping, .. } = &mut model.nodes[1].kind else { unreachable!() };
        output_mapping.insert("answer".into(), "[".into());
        assert!(validate_draft(&model).is_err(), "a provided invalid mapping cannot be saved");
        let ProcessNodeKind::ScriptTask { output_mapping, .. } = &mut model.nodes[1].kind else { unreachable!() };
        output_mapping.insert("answer".into(), "outputs".into());
        model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 1 },
            output_collection_variable: "answer".into(),
        });
        assert!(validate_draft(&model).unwrap_err().to_string().contains("requires a UserTask or ServiceTask"));
    }

    #[test]
    fn script_task_is_immediate_before_a_real_escalation_wait() {
        let mut model = escalation_model();
        model.nodes.push(ProcessNode { id: "Script_1".into(), name: "Prepare".into(), repeat: None,
            kind: ProcessNodeKind::ScriptTask { script: "null".into(), output_mapping: BTreeMap::new() } });
        model.sequence_flows.iter_mut().find(|flow| flow.id == "Flow_Escalation").unwrap().target_id = "Script_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_Script_Wait".into(),
            source_id: "Script_1".into(), target_id: "Wait_1".into(), condition: None });
        validate_model(&model).unwrap();
    }

    #[test]
    fn manual_task_requires_publishable_external_instructions_without_repeat() {
        let mut model = starter_model();
        model.nodes.insert(1, ProcessNode { id: "Manual_1".into(), name: "External work".into(), repeat: None,
            kind: ProcessNodeKind::ManualTask { assignee_user_id: None, instructions: String::new() } });
        model.sequence_flows[0].target_id = "Manual_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_2".into(),
            source_id: "Manual_1".into(), target_id: "End_1".into(), condition: None });
        validate_draft(&model).unwrap();
        assert!(validate_model(&model).unwrap_err().to_string().contains("requires instructions"));
        model.nodes[1].kind = ProcessNodeKind::ManualTask { assignee_user_id: Some("worker-2".into()),
            instructions: "Check the external register.\nAcknowledge here.".into() };
        validate_model(&model).unwrap();
        model.nodes[1].kind = ProcessNodeKind::ManualTask { assignee_user_id: Some("worker-2".into()),
            instructions: "invalid\rline".into() };
        assert!(validate_draft(&model).unwrap_err().to_string().contains("invalid instructions"));
        model.nodes[1].kind = ProcessNodeKind::ManualTask { assignee_user_id: Some("worker-2".into()),
            instructions: "x".repeat(4097) };
        assert!(validate_model(&model).unwrap_err().to_string().contains("invalid instructions"));
        model.nodes[1].kind = ProcessNodeKind::ManualTask { assignee_user_id: Some("worker-2".into()),
            instructions: "Check the external register".into() };
        model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
        assert!(validate_draft(&model).unwrap_err().to_string().contains("requires a UserTask or ServiceTask"));
    }

    #[test]
    fn send_and_receive_tasks_require_declared_messages_and_refuse_direct_repeat() {
        let mut model = starter_model();
        model.messages.push(tentaflow_protocol::processes::ProcessMessageDeclaration {
            message_id: "Message_1".into(), name: "order.received".into(),
        });
        model.nodes.insert(1, ProcessNode { id: "Send_1".into(), name: "Admit locally".into(), repeat: None,
            kind: ProcessNodeKind::SendTask { message_ref: "Message_1".into(),
                target: ProcessMessageTargetSpec::Start { definition_id: uuid::Uuid::nil().to_string() },
                correlation_expression: "vars.case_key".into(), payload_expression: "vars.payload".into(),
                ttl_seconds: 60 } });
        model.nodes.insert(2, ProcessNode { id: "Receive_1".into(), name: "Wait".into(), repeat: None,
            kind: ProcessNodeKind::ReceiveTask { message_ref: "Message_1".into(),
                correlation_expression: "vars.case_key".into(), output_mapping: BTreeMap::new() } });
        model.sequence_flows[0].target_id = "Send_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_Send".into(),
            source_id: "Send_1".into(), target_id: "Receive_1".into(), condition: None });
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_Receive".into(),
            source_id: "Receive_1".into(), target_id: "End_1".into(), condition: None });
        model.variables.insert("case_key".into(), serde_json::Value::String("case-1".into()));
        model.variables.insert("payload".into(), serde_json::json!({"opaque_key": 1}));
        validate_model(&model).unwrap();
        model.nodes[2].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
        assert!(validate_draft(&model).unwrap_err().to_string().contains("requires a UserTask or ServiceTask"));
        model.nodes[2].repeat = None;
        if let ProcessNodeKind::SendTask { message_ref, .. } = &mut model.nodes[1].kind {
            *message_ref = "Missing".into();
        }
        assert!(validate_model(&model).unwrap_err().to_string().contains("unknown declaration"));
    }

    #[test]
    fn signals_require_exact_namespace_declarations_and_exclude_direct_repeat() {
        let mut model = starter_model();
        model.target_namespace = Some("urn:orders".into());
        model.signals.push(tentaflow_protocol::processes::ProcessSignalDeclaration {
            signal_id: "Signal_1".into(), namespace_uri: "urn:orders".into(), name: "Order changed".into(),
        });
        model.variables.insert("payload".into(), serde_json::json!({"business_key": 1}));
        model.nodes.insert(1, ProcessNode { id: "Throw_1".into(), name: "Admit signal".into(), repeat: None,
            kind: ProcessNodeKind::SignalThrow { signal_ref: "Signal_1".into(),
                payload_expression: "vars.payload".into(), ttl_seconds: 60 } });
        model.nodes.insert(2, ProcessNode { id: "Catch_1".into(), name: "Wait for signal".into(), repeat: None,
            kind: ProcessNodeKind::SignalCatch { signal_ref: "Signal_1".into(),
                output_mapping: BTreeMap::new() } });
        model.sequence_flows[0].target_id = "Throw_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_Throw".into(),
            source_id: "Throw_1".into(), target_id: "Catch_1".into(), condition: None });
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_Catch".into(),
            source_id: "Catch_1".into(), target_id: "End_1".into(), condition: None });
        validate_model(&model).unwrap();
        model.target_namespace = None;
        assert!(validate_model(&model).unwrap_err().to_string().contains("explicit target namespace"));
        model.target_namespace = Some("urn:orders".into());
        model.signals[0].namespace_uri = "urn:foreign".into();
        assert!(validate_model(&model).unwrap_err().to_string().contains("namespace differs"));
        model.signals[0].namespace_uri = "urn:orders".into();
        model.nodes[2].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
        assert!(validate_draft(&model).unwrap_err().to_string().contains("requires a UserTask or ServiceTask"));
        model.nodes[2].repeat = None;
        if let ProcessNodeKind::SignalThrow { ttl_seconds, .. } = &mut model.nodes[1].kind { *ttl_seconds = 0; }
        assert!(validate_model(&model).unwrap_err().to_string().contains("TTL"));
    }
}
