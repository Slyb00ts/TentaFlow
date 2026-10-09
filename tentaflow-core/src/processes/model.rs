// ============ File: model.rs — Process graph validation and structured gateway joins ============

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use tentaflow_protocol::processes::{
    ProcessBodyModeling, ProcessCallTarget, ProcessDiagram, ProcessEscalationPathReason,
    ProcessMessageTargetSpec, ProcessModel, ProcessMultiInstanceInput, ProcessNode,
    ProcessNodeKind, ProcessRepeatSpec, ProcessSequenceFlow, ProcessTimerSpec,
};

use crate::flow_engine::expr;
use crate::project_studio::schedules::parse_timezone;

pub const MAX_MODEL_BYTES: usize = 512 * 1024;
pub const MAX_NODES: usize = 128;
pub const MAX_SEQUENCE_FLOWS: usize = 256;
pub const MAX_VARIABLE_BYTES: usize = 256 * 1024;
pub const MAX_VARIABLE_KEYS: usize = 128;
pub const MAX_DATA_STORE_CAPACITY: i64 = 9_007_199_254_740_991;
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
) -> Result<(
    &'a [ProcessNode],
    &'a [ProcessSequenceFlow],
    &'a BTreeMap<String, serde_json::Value>,
)> {
    let mut nodes = model.nodes.as_slice();
    let mut flows = model.sequence_flows.as_slice();
    let mut variables = &model.variables;
    for node_id in subprocess_node_ids {
        let node = nodes
            .iter()
            .find(|node| node.id == *node_id)
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

pub struct SelectedProcessBody<'a> {
    pub nodes: &'a [ProcessNode],
    pub sequence_flows: &'a [ProcessSequenceFlow],
    pub variables: &'a BTreeMap<String, serde_json::Value>,
    pub diagram: &'a ProcessDiagram,
    pub modeling: Option<&'a ProcessBodyModeling>,
}

/// Body modeling for activity IO evaluation. A body that authors only CEL
/// associations carries no modeling; direct references to absent objects then
/// fail through their own association instead of rejecting the whole body.
pub(super) fn io_modeling(modeling: Option<&ProcessBodyModeling>) -> &ProcessBodyModeling {
    static EMPTY: std::sync::OnceLock<ProcessBodyModeling> = std::sync::OnceLock::new();
    modeling.unwrap_or_else(|| EMPTY.get_or_init(ProcessBodyModeling::default))
}

pub fn selected_body<'a>(
    model: &'a ProcessModel,
    process_id: &str,
    subprocess_node_ids: &[String],
) -> Result<SelectedProcessBody<'a>> {
    let mut body = if model.process_id == process_id {
        SelectedProcessBody {
            nodes: &model.nodes,
            sequence_flows: &model.sequence_flows,
            variables: &model.variables,
            diagram: &model.diagram,
            modeling: model.modeling.as_ref(),
        }
    } else {
        let process = model
            .additional_processes
            .iter()
            .find(|process| process.process_id == process_id)
            .with_context(|| format!("process {process_id} is not in the pinned model"))?;
        SelectedProcessBody {
            nodes: &process.nodes,
            sequence_flows: &process.sequence_flows,
            variables: &process.variables,
            diagram: &process.diagram,
            modeling: process.modeling.as_ref(),
        }
    };
    for node_id in subprocess_node_ids {
        let node = body
            .nodes
            .iter()
            .find(|node| node.id == *node_id)
            .with_context(|| format!("subprocess {node_id} is outside its selected parent body"))?;
        let ProcessNodeKind::SubProcess { body: child, .. } = &node.kind else {
            bail!("scope path node {node_id} is not a subprocess");
        };
        body = SelectedProcessBody {
            nodes: &child.nodes,
            sequence_flows: &child.sequence_flows,
            variables: &child.variables,
            diagram: &child.diagram,
            modeling: child.modeling.as_ref(),
        };
    }
    Ok(body)
}

fn visit_nodes<'a>(nodes: &'a [ProcessNode], result: &mut Vec<&'a ProcessNode>) {
    for node in nodes {
        result.push(node);
        if let ProcessNodeKind::SubProcess { body, .. } = &node.kind {
            visit_nodes(&body.nodes, result);
        }
    }
}

pub fn all_nodes_in_body<'a>(
    model: &'a ProcessModel,
    process_id: &str,
) -> Result<Vec<&'a ProcessNode>> {
    let body = selected_body(model, process_id, &[])?;
    let mut nodes = Vec::new();
    visit_nodes(body.nodes, &mut nodes);
    Ok(nodes)
}

pub fn all_nodes(model: &ProcessModel) -> Vec<&ProcessNode> {
    let mut nodes = Vec::new();
    visit_nodes(&model.nodes, &mut nodes);
    for process in &model.additional_processes {
        visit_nodes(&process.nodes, &mut nodes);
    }
    nodes
}

pub fn starter_model() -> ProcessModel {
    use tentaflow_protocol::processes::{ProcessDiagram, ProcessNode, ProcessSequenceFlow};
    ProcessModel {
        schema_version: 1,
        process_id: "Process_1".into(),
        nodes: vec![
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Start_1".into(),
                name: "Start".into(),
                kind: ProcessNodeKind::Start,
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "End_1".into(),
                name: "End".into(),
                kind: ProcessNodeKind::End,
            },
        ],
        sequence_flows: vec![ProcessSequenceFlow {
            call_start_node_id: None,
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
        process_name: None,
        additional_processes: Vec::new(),
        modeling: None,
        collaboration: None,
        data_stores: Vec::new(),
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
        model.messages.len() <= 32
            && model.errors.len() <= 32
            && model.escalations.len() <= 32
            && model.signals.len() <= 32,
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
                    .all(|byte| byte.is_ascii_alphanumeric()
                        || matches!(byte, b'_' | b'.' | b':' | b'-'))
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
        ensure!(
            model.target_namespace.is_some(),
            "signal declarations require an explicit target namespace"
        );
    }
    for signal in &model.signals {
        ensure!(
            valid_id(&signal.signal_id) && ids.insert(signal.signal_id.as_str()),
            "invalid or duplicate BPMN ID: {}",
            signal.signal_id
        );
        ensure!(
            signal.namespace_uri == model.target_namespace.as_deref().unwrap_or_default(),
            "signal {} namespace differs from the process target namespace",
            signal.signal_id
        );
        ensure!(
            !signal.name.is_empty()
                && signal.name.len() <= 256
                && !signal.name.chars().any(char::is_control),
            "invalid signal name: {}",
            signal.signal_id
        );
    }
    Ok(())
}

fn validate_expression(expression: &str, context: &str, required: bool) -> Result<()> {
    if expression.is_empty() && !required {
        return Ok(());
    }
    ensure!(
        !expression.is_empty() && expression.len() <= 4096,
        "invalid {context} length"
    );
    expr::validate_syntax(expression, None).with_context(|| context.to_string())
}

fn validate_message_target(target: &ProcessMessageTargetSpec, complete: bool) -> Result<()> {
    match target {
        ProcessMessageTargetSpec::Start {
            definition_id,
            process_id,
            start_node_id,
        } => {
            if complete || !definition_id.is_empty() {
                uuid::Uuid::parse_str(definition_id)
                    .context("invalid message start target definition")?;
            }
            if let Some(process_id) = process_id {
                ensure!(
                    valid_id(process_id),
                    "invalid message start target process ID"
                );
            }
            if let Some(start_node_id) = start_node_id {
                ensure!(
                    valid_id(start_node_id),
                    "invalid message start target node ID"
                );
                ensure!(
                    process_id.is_some(),
                    "message start target node requires a process ID"
                );
            }
        }
        ProcessMessageTargetSpec::Catch {
            definition_id,
            instance_id_expression,
            subscription_id_expression,
        } => {
            if complete || !definition_id.is_empty() {
                uuid::Uuid::parse_str(definition_id)
                    .context("invalid message catch target definition")?;
            }
            ensure!(
                subscription_id_expression.is_none() || instance_id_expression.is_some(),
                "subscription target requires an instance expression"
            );
            if let Some(expression) = instance_id_expression {
                validate_expression(expression, "message target instance expression", complete)?;
            }
            if let Some(expression) = subscription_id_expression {
                validate_expression(
                    expression,
                    "message target subscription expression",
                    complete,
                )?;
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
            ensure!(
                is_start,
                "repeating timer is only supported as a start event"
            );
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
            ensure!(
                is_start,
                "repeating timer is only supported as a start event"
            );
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
        ensure!(
            bytes.len() <= 128 * 1024,
            "process calendar and pin exceed 128 KiB"
        );
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

fn validate_repeat(
    node: &ProcessNode,
    variables: &BTreeMap<String, serde_json::Value>,
    complete: bool,
) -> Result<()> {
    let Some(spec) = &node.repeat else {
        return Ok(());
    };
    let output_mapping = match &node.kind {
        ProcessNodeKind::UserTask { output_mapping, .. }
        | ProcessNodeKind::ServiceTask { output_mapping, .. }
        | ProcessNodeKind::ReceiveTask { output_mapping, .. }
        | ProcessNodeKind::SubProcess { output_mapping, .. }
        | ProcessNodeKind::ScriptTask { output_mapping, .. } => Some(output_mapping),
        ProcessNodeKind::CallActivity(call) => Some(&call.output_mapping),
        ProcessNodeKind::ManualTask { .. } | ProcessNodeKind::SendTask { .. } => None,
        _ => bail!("repeat on {} requires a supported activity", node.id),
    };
    let output_variable = match spec {
        ProcessRepeatSpec::MultiInstance {
            input,
            output_collection_variable,
            ..
        } => {
            if complete && matches!(node.kind, ProcessNodeKind::ScriptTask { .. }) {
                ensure!(
                    output_mapping.is_some_and(BTreeMap::is_empty),
                    "repeat {} ScriptTask MI requires empty output mapping",
                    node.id
                );
            }
            match input {
                ProcessMultiInstanceInput::Cardinality { count } => {
                    ensure!(*count <= 16, "repeat {} cardinality exceeds 16", node.id);
                }
                ProcessMultiInstanceInput::CollectionExpression { expression } => {
                    validate_expression(
                        expression,
                        &format!("repeat {} collection expression", node.id),
                        complete,
                    )?;
                }
            }
            output_collection_variable
        }
        ProcessRepeatSpec::StructuredLoop {
            condition,
            max_iterations,
            output_collection_variable,
            ..
        } => {
            ensure!(
                (1..=32).contains(max_iterations),
                "repeat {} loop maximum outside 1..=32",
                node.id
            );
            validate_expression(
                condition,
                &format!("repeat {} loop condition", node.id),
                complete,
            )?;
            output_collection_variable
        }
    };
    if complete || !output_variable.is_empty() {
        ensure!(
            valid_id(output_variable),
            "repeat {} has invalid output collection variable",
            node.id
        );
        ensure!(
            !output_mapping.is_some_and(|mapping| mapping.contains_key(output_variable)),
            "repeat {} output collection conflicts with task mapping",
            node.id
        );
        if complete {
            ensure!(
                variables.contains_key(output_variable),
                "repeat {} output collection variable is undeclared",
                node.id
            );
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
        model.additional_processes.len() <= 15,
        "too many executable process bodies"
    );
    ensure!(
        serde_json::to_vec(model)?.len() <= MAX_MODEL_BYTES,
        "process draft exceeds 512 KiB"
    );
    validate_timer_model(model)?;
    let mut all_ids = HashSet::from([model.process_id.as_str()]);
    validate_declarations(model, &mut all_ids)?;
    let mut data_stores = HashSet::new();
    for store in &model.data_stores {
        ensure!(
            valid_id(&store.id) && all_ids.insert(store.id.as_str()),
            "invalid or duplicate data store ID: {}",
            store.id
        );
        ensure!(
            store.name.as_deref().is_none_or(|name| name.len() <= 256
                && !name.chars().any(char::is_control)),
            "invalid data store name"
        );
        ensure!(store.capacity.is_none_or(|capacity| (0..=MAX_DATA_STORE_CAPACITY).contains(&capacity)),
            "invalid data store capacity");
        data_stores.insert(store.id.as_str());
    }
    let mut node_count = 0;
    let mut flow_count = 0;
    validate_draft_body(
        &model.nodes,
        &model.sequence_flows,
        &model.variables,
        &model.diagram,
        model.modeling.as_ref(),
        &data_stores,
        0,
        &mut all_ids,
        &mut node_count,
        &mut flow_count,
    )?;
    if let Some(name) = &model.process_name {
        ensure!(
            name.len() <= 256 && !name.chars().any(char::is_control),
            "invalid primary process name"
        );
    }
    for process in &model.additional_processes {
        ensure!(
            valid_id(&process.process_id) && all_ids.insert(process.process_id.as_str()),
            "invalid or duplicate BPMN ID: {}",
            process.process_id
        );
        if let Some(name) = &process.process_name {
            ensure!(
                name.len() <= 256 && !name.chars().any(char::is_control),
                "invalid additional process name"
            );
        }
        let mut body_model = model.clone();
        body_model.nodes = process.nodes.clone();
        body_model.timer_timezone = process.timer_timezone.clone();
        body_model.work_calendar = process.work_calendar.clone();
        body_model.calendar_pin = process.calendar_pin.clone();
        validate_timer_model(&body_model)?;
        validate_draft_body(
            &process.nodes,
            &process.sequence_flows,
            &process.variables,
            &process.diagram,
            process.modeling.as_ref(),
            &data_stores,
            0,
            &mut all_ids,
            &mut node_count,
            &mut flow_count,
        )
        .with_context(|| format!("process {}", process.process_id))?;
    }
    validate_escalation_prefixes(&model.nodes, &model.sequence_flows, false)?;
    for process in &model.additional_processes {
        validate_escalation_prefixes(&process.nodes, &process.sequence_flows, false)?;
    }
    validate_collaboration(model, &mut all_ids)?;
    Ok(())
}

fn validate_draft_body<'a>(
    nodes: &'a [ProcessNode],
    flows: &'a [ProcessSequenceFlow],
    variables: &BTreeMap<String, serde_json::Value>,
    diagram: &'a ProcessDiagram,
    modeling: Option<&'a ProcessBodyModeling>,
    data_stores: &HashSet<&str>,
    depth: usize,
    all_ids: &mut HashSet<&'a str>,
    node_count: &mut usize,
    flow_count: &mut usize,
) -> Result<()> {
    ensure!(depth <= 3, "embedded subprocess depth exceeds three levels");
    *node_count += nodes.len();
    *flow_count += flows.len();
    ensure!(
        *node_count <= MAX_NODES && *flow_count <= MAX_SEQUENCE_FLOWS,
        "process draft exceeds whole-tree graph limits"
    );
    validate_variables(&serde_json::to_value(variables)?)?;
    for node in nodes {
        ensure!(
            valid_id(&node.id) && all_ids.insert(node.id.as_str()),
            "invalid or duplicate BPMN ID: {}",
            node.id
        );
        ensure!(
            node.name.len() <= 256 && !node.name.chars().any(char::is_control),
            "invalid node name"
        );
        validate_repeat(node, variables, false)?;
        if let ProcessNodeKind::LinkThrow { definition }
        | ProcessNodeKind::LinkCatch { definition } = &node.kind
        {
            ensure!(
                node.repeat.is_none()
                    && valid_id(&definition.id)
                    && all_ids.insert(definition.id.as_str())
                    && definition.name.len() <= 256
                    && !definition.name.chars().any(char::is_control),
                "invalid or duplicate Link event definition at {}",
                node.id
            );
        }
        validate_activity_io(node, modeling, variables, all_ids, false)?;
        match &node.kind {
            ProcessNodeKind::ManualTask {
                assignee_user_id,
                instructions,
            } => {
                ensure!(
                    valid_manual_instructions(instructions),
                    "manual task {} has invalid instructions",
                    node.id
                );
                ensure!(
                    assignee_user_id
                        .as_ref()
                        .map_or(true, |user| !user.is_empty()),
                    "manual task {} has empty assignee",
                    node.id
                );
            }
            ProcessNodeKind::ScriptTask {
                script,
                output_mapping,
            } => {
                ensure!(
                    script.len() <= 4096,
                    "script task {} expression is too long",
                    node.id
                );
                if !script.is_empty() {
                    expr::validate_script_profile(script)
                        .with_context(|| format!("script task {} expression", node.id))?;
                }
                validate_mapping(output_mapping, true)?;
            }
            ProcessNodeKind::ServiceTask {
                input_mapping,
                output_mapping,
                verification,
                timeout_seconds,
                result_expression,
                ..
            } => {
                ensure!(
                    (1..=600).contains(timeout_seconds),
                    "service timeout outside 1..=600 seconds"
                );
                validate_mapping(input_mapping, false)?;
                validate_mapping(output_mapping, false)?;
                if let tentaflow_protocol::processes::ActivityVerification::Condition {
                    expression,
                } = verification
                {
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
            ProcessNodeKind::SignalThrow {
                payload_expression,
                ttl_seconds,
                ..
            } => {
                validate_expression(payload_expression, "signal payload expression", false)?;
                ensure!(
                    (1..=604_800).contains(ttl_seconds),
                    "signal TTL outside 1..=604800 seconds"
                );
            }
            ProcessNodeKind::MessageThrow {
                target,
                correlation_expression,
                payload_expression,
                ttl_seconds,
                ..
            }
            | ProcessNodeKind::SendTask {
                target,
                correlation_expression,
                payload_expression,
                ttl_seconds,
                ..
            } => {
                validate_message_target(target, false)?;
                validate_expression(
                    correlation_expression,
                    "message correlation expression",
                    false,
                )?;
                validate_expression(payload_expression, "message payload expression", false)?;
                ensure!(
                    (1..=604_800).contains(ttl_seconds),
                    "message TTL outside 1..=604800 seconds"
                );
            }
            ProcessNodeKind::SubProcess {
                body,
                input_mapping,
                output_mapping,
            } => {
                validate_mapping(input_mapping, false)?;
                validate_mapping(output_mapping, false)?;
                validate_draft_body(
                    &body.nodes,
                    &body.sequence_flows,
                    &body.variables,
                    &body.diagram,
                    body.modeling.as_ref(),
                    data_stores,
                    depth + 1,
                    all_ids,
                    node_count,
                    flow_count,
                )?;
            }
            ProcessNodeKind::CallActivity(call) => {
                let called_element = match &call.target {
                    ProcessCallTarget::PublishedBody {
                        definition_id,
                        version,
                        called_element,
                    } => {
                        if !definition_id.is_empty() {
                            uuid::Uuid::parse_str(definition_id).with_context(|| {
                                format!("call activity {} has invalid target definition", node.id)
                            })?;
                        }
                        ensure!(
                            *version == 0 || !definition_id.is_empty(),
                            "call activity {} has a version without a target",
                            node.id
                        );
                        called_element
                    }
                    ProcessCallTarget::LocalBody { called_element } => called_element,
                };
                if !called_element.namespace_uri.is_empty() {
                    ensure!(
                        called_element.namespace_uri.len() <= 1024
                            && !called_element
                                .namespace_uri
                                .chars()
                                .any(char::is_whitespace)
                            && !called_element.namespace_uri.chars().any(char::is_control),
                        "call activity {} has invalid namespace",
                        node.id
                    );
                    url::Url::parse(&called_element.namespace_uri).with_context(|| {
                        format!("call activity {} has invalid namespace", node.id)
                    })?;
                }
                ensure!(
                    called_element.process_id.is_empty() || valid_id(&called_element.process_id),
                    "call activity {} has invalid process ID",
                    node.id
                );
                validate_mapping(&call.input_mapping, false)?;
                validate_mapping(&call.output_mapping, false)?;
            }
            _ => {}
        }
        if depth > 0 {
            ensure!(
                !matches!(
                    node.kind,
                    ProcessNodeKind::TimerStart { .. } | ProcessNodeKind::MessageStart { .. }
                ),
                "timer and message starts are root-only: {}",
                node.id
            );
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
            if let Some(attached) = nodes
                .iter()
                .find(|attached| attached.id == *attached_to_id && attached.repeat.is_some())
            {
                let supported = matches!(
                    node.kind,
                    ProcessNodeKind::BoundaryTimer { .. } | ProcessNodeKind::BoundaryMessage { .. }
                ) || matches!(node.kind, ProcessNodeKind::BoundaryError { .. })
                    && matches!(
                        attached.kind,
                        ProcessNodeKind::ServiceTask { .. }
                            | ProcessNodeKind::SubProcess { .. }
                            | ProcessNodeKind::CallActivity(..)
                    )
                    || matches!(node.kind, ProcessNodeKind::BoundaryEscalation { .. })
                        && matches!(attached.kind, ProcessNodeKind::ServiceTask { .. });
                ensure!(
                    supported,
                    "repeat {} has an unsupported boundary event",
                    attached_to_id
                );
            }
        }
    }
    let mut flow_ids = HashSet::new();
    for flow in flows {
        ensure!(
            valid_id(&flow.id)
                && flow_ids.insert(flow.id.as_str())
                && all_ids.insert(flow.id.as_str()),
            "invalid or duplicate BPMN ID: {}",
            flow.id
        );
        if let Some(expression) = &flow.condition {
            validate_expression(expression, "sequence flow condition", true)?;
        }
    }
    let node_map = nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    validate_diagram(diagram, nodes, flows, &node_map, &flow_ids)?;
    validate_modeling(modeling, diagram, nodes, variables, data_stores, all_ids)
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
    let signal_ids: HashSet<_> = model
        .signals
        .iter()
        .map(|signal| signal.signal_id.as_str())
        .collect();
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
    validate_complete_io(&model.nodes, model.modeling.as_ref(), &model.variables)?;
    for process in &model.additional_processes {
        validate_body(
            &process.nodes,
            &process.sequence_flows,
            &process.diagram,
            &process.variables,
            0,
            &message_ids,
            &error_ids,
            &escalation_ids,
            &signal_ids,
            &mut used_messages,
            &mut used_errors,
            &mut used_escalations,
            &mut used_signals,
        )
        .with_context(|| format!("process {}", process.process_id))?;
        validate_complete_io(
            &process.nodes,
            process.modeling.as_ref(),
            &process.variables,
        )?;
    }
    validate_local_calls(model)?;
    ensure!(
        message_ids == used_messages,
        "unreferenced message declaration"
    );
    ensure!(error_ids == used_errors, "unreferenced error declaration");
    ensure!(
        escalation_ids == used_escalations,
        "unreferenced escalation declaration"
    );
    ensure!(
        signal_ids == used_signals,
        "unreferenced signal declaration"
    );
    Ok(())
}

fn validate_local_calls(model: &ProcessModel) -> Result<()> {
    let namespace = model
        .target_namespace
        .as_deref()
        .unwrap_or("https://tentaflow.app/bpmn/1");
    let mut local_edges: HashMap<&str, Vec<&str>> = HashMap::new();
    for process_id in std::iter::once(model.process_id.as_str()).chain(
        model
            .additional_processes
            .iter()
            .map(|process| process.process_id.as_str()),
    ) {
        let mut paths = vec![Vec::<String>::new()];
        while let Some(path) = paths.pop() {
            let body = selected_body(model, process_id, &path)?;
            for node in body.nodes {
                if matches!(node.kind, ProcessNodeKind::SubProcess { .. }) {
                    let mut child = path.clone();
                    child.push(node.id.clone());
                    paths.push(child);
                }
                let ProcessNodeKind::CallActivity(call) = &node.kind else {
                    continue;
                };
                if let ProcessCallTarget::LocalBody { called_element } = &call.target {
                    ensure!(
                        called_element.namespace_uri == namespace,
                        "local call {} must target this document namespace",
                        node.id
                    );
                    selected_body(model, &called_element.process_id, &[])
                        .with_context(|| format!("local call {} has no target body", node.id))?;
                    local_edges
                        .entry(process_id)
                        .or_default()
                        .push(&called_element.process_id);
                }
                let incoming: Vec<_> = body
                    .sequence_flows
                    .iter()
                    .filter(|flow| flow.target_id == node.id)
                    .collect();
                if let ProcessCallTarget::LocalBody { called_element } = &call.target {
                    let target = selected_body(model, &called_element.process_id, &[])?;
                    let none_starts = target
                        .nodes
                        .iter()
                        .filter(|start| matches!(start.kind, ProcessNodeKind::Start))
                        .count();
                    if none_starts > 1 {
                        ensure!(
                            incoming
                                .iter()
                                .all(|flow| flow.call_start_node_id.is_some()),
                            "local call {} requires an exact Start for each incoming flow",
                            node.id
                        );
                    }
                }
                for flow in incoming {
                    if let Some(start_id) = &flow.call_start_node_id {
                        let ProcessCallTarget::LocalBody { called_element } = &call.target else {
                            continue;
                        };
                        let target = selected_body(model, &called_element.process_id, &[])?;
                        ensure!(
                            target.nodes.iter().any(|start| start.id == *start_id
                                && matches!(start.kind, ProcessNodeKind::Start)),
                            "call flow {} names no None Start in its pinned local body",
                            flow.id
                        );
                    }
                }
            }
            for flow in body.sequence_flows {
                if flow.call_start_node_id.is_some() {
                    ensure!(
                        body.nodes.iter().any(|node| node.id == flow.target_id
                            && matches!(node.kind, ProcessNodeKind::CallActivity(..))),
                        "sequence flow {} has a Call Start selector outside a CallActivity",
                        flow.id
                    );
                }
            }
        }
    }
    for origin in local_edges.keys().copied() {
        let mut visited = HashSet::new();
        let mut pending = vec![origin];
        while let Some(process_id) = pending.pop() {
            for target in local_edges.get(process_id).into_iter().flatten() {
                ensure!(*target != origin, "local Call body cycle reaches {origin}");
                if visited.insert(*target) {
                    pending.push(*target);
                }
            }
        }
    }
    Ok(())
}

fn validate_complete_io(
    nodes: &[ProcessNode],
    modeling: Option<&ProcessBodyModeling>,
    variables: &BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    for node in nodes {
        let mut ids = HashSet::new();
        validate_activity_io(node, modeling, variables, &mut ids, true)?;
        if let ProcessNodeKind::SubProcess { body, .. } = &node.kind {
            validate_complete_io(&body.nodes, body.modeling.as_ref(), &body.variables)?;
        }
    }
    Ok(())
}

pub fn link_pairs<'a>(nodes: &'a [ProcessNode]) -> Result<HashMap<&'a str, &'a str>> {
    let mut catches = HashMap::new();
    let mut throws: HashMap<&str, Vec<(&str, &str, Option<&str>)>> = HashMap::new();
    for node in nodes {
        match &node.kind {
            ProcessNodeKind::LinkCatch { definition } => {
                ensure!(!definition.name.is_empty(),
                    "Link Catch {} requires a name", node.id);
                ensure!(definition.target_ref.is_none(),
                    "Link Catch {} cannot name a target", node.id);
                ensure!(catches.insert(definition.name.as_str(),
                    (node.id.as_str(), definition.id.as_str(), definition.source_refs.as_slice())).is_none(),
                    "duplicate Link Catch name {} in one body", definition.name);
            }
            ProcessNodeKind::LinkThrow { definition } => {
                ensure!(!definition.name.is_empty(),
                    "Link Throw {} requires a name", node.id);
                ensure!(definition.source_refs.is_empty(),
                    "Link Throw {} cannot name sources", node.id);
                throws.entry(&definition.name).or_default().push((
                    &node.id, &definition.id, definition.target_ref.as_deref()));
            }
            _ => {}
        }
    }
    let mut pairs = HashMap::new();
    for (name, sources) in &throws {
        let (catch_id, catch_definition, declared_sources) = catches.get(name)
            .with_context(|| format!("Link Throw {name} has no Catch in its body"))?;
        let actual_sources = sources.iter().map(|(_, id, _)| *id).collect::<HashSet<_>>();
        if !declared_sources.is_empty() {
            ensure!(declared_sources.iter().map(String::as_str).collect::<HashSet<_>>()
                == actual_sources && declared_sources.len() == sources.len(),
                "Link Catch {catch_id} source references differ from its Throws");
        }
        for (throw_id, _, target) in sources {
            ensure!(target.is_none_or(|id| id == *catch_definition),
                "Link Throw {throw_id} target differs from its Catch");
            pairs.insert(*throw_id, *catch_id);
        }
    }
    for (name, (catch_id, _, _)) in catches {
        ensure!(throws.contains_key(name),
            "Link Catch {catch_id} has no Throw in its body");
    }
    Ok(pairs)
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
            ProcessNodeKind::ManualTask {
                assignee_user_id,
                instructions,
            } => {
                ensure!(
                    !instructions.is_empty(),
                    "manual task {} requires instructions",
                    node.id
                );
                ensure!(
                    valid_manual_instructions(instructions),
                    "manual task {} has invalid instructions",
                    node.id
                );
                ensure!(
                    assignee_user_id
                        .as_ref()
                        .map_or(true, |user| !user.is_empty()),
                    "manual task {} has empty assignee",
                    node.id
                );
            }
            ProcessNodeKind::ScriptTask {
                script,
                output_mapping,
            } => {
                ensure!(
                    !script.is_empty(),
                    "script task {} requires an expression",
                    node.id
                );
                ensure!(
                    script.len() <= 4096,
                    "script task {} expression is too long",
                    node.id
                );
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
            ProcessNodeKind::MessageStart {
                message_ref,
                output_mapping,
            } => {
                ensure!(
                    message_ids.contains(message_ref.as_str()),
                    "message start {} references an unknown declaration",
                    node.id
                );
                used_messages.insert(message_ref.as_str());
                validate_mapping(output_mapping, false)?;
            }
            ProcessNodeKind::MessageCatch {
                message_ref,
                correlation_expression,
                output_mapping,
            }
            | ProcessNodeKind::ReceiveTask {
                message_ref,
                correlation_expression,
                output_mapping,
            }
            | ProcessNodeKind::BoundaryMessage {
                message_ref,
                correlation_expression,
                output_mapping,
                ..
            } => {
                ensure!(
                    message_ids.contains(message_ref.as_str()),
                    "message event {} references an unknown declaration",
                    node.id
                );
                used_messages.insert(message_ref.as_str());
                validate_expression(
                    correlation_expression,
                    "message correlation expression",
                    true,
                )?;
                validate_mapping(output_mapping, false)?;
            }
            ProcessNodeKind::MessageThrow {
                message_ref,
                target,
                correlation_expression,
                payload_expression,
                ttl_seconds,
            }
            | ProcessNodeKind::SendTask {
                message_ref,
                target,
                correlation_expression,
                payload_expression,
                ttl_seconds,
            } => {
                ensure!(
                    message_ids.contains(message_ref.as_str()),
                    "message throw {} references an unknown declaration",
                    node.id
                );
                used_messages.insert(message_ref.as_str());
                validate_message_target(target, true)?;
                validate_expression(
                    correlation_expression,
                    "message correlation expression",
                    true,
                )?;
                validate_expression(payload_expression, "message payload expression", true)?;
                ensure!(
                    (1..=604_800).contains(ttl_seconds),
                    "message TTL outside 1..=604800 seconds"
                );
            }
            ProcessNodeKind::SignalCatch {
                signal_ref,
                output_mapping,
            } => {
                ensure!(
                    signal_ids.contains(signal_ref.as_str()),
                    "signal catch {} references an unknown declaration",
                    node.id
                );
                used_signals.insert(signal_ref.as_str());
                validate_mapping(output_mapping, false)?;
            }
            ProcessNodeKind::SignalThrow {
                signal_ref,
                payload_expression,
                ttl_seconds,
            } => {
                ensure!(
                    signal_ids.contains(signal_ref.as_str()),
                    "signal throw {} references an unknown declaration",
                    node.id
                );
                used_signals.insert(signal_ref.as_str());
                validate_expression(payload_expression, "signal payload expression", true)?;
                ensure!(
                    (1..=604_800).contains(ttl_seconds),
                    "signal TTL outside 1..=604800 seconds"
                );
            }
            ProcessNodeKind::BoundaryError {
                attached_to_id,
                error_ref,
                output_mapping,
            } => {
                if let Some(reference) = error_ref {
                    ensure!(
                        error_ids.contains(reference.as_str()),
                        "boundary error {} references an unknown declaration",
                        node.id
                    );
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
            ProcessNodeKind::CallActivity(call) => {
                let called_element = match &call.target {
                    ProcessCallTarget::PublishedBody {
                        definition_id,
                        version,
                        called_element,
                    } => {
                        uuid::Uuid::parse_str(definition_id).with_context(|| {
                            format!("call activity {} has invalid target definition", node.id)
                        })?;
                        ensure!(
                            *version > 0,
                            "call activity {} requires an exact published target version",
                            node.id
                        );
                        called_element
                    }
                    ProcessCallTarget::LocalBody { called_element } => called_element,
                };
                ensure!(
                    valid_id(&called_element.process_id)
                        && !called_element.namespace_uri.is_empty(),
                    "call activity {} requires an exact target QName",
                    node.id
                );
                validate_mapping(&call.input_mapping, false)?;
                validate_mapping(&call.output_mapping, false)?;
            }
            ProcessNodeKind::ErrorEnd { error_ref } => {
                ensure!(
                    error_ids.contains(error_ref.as_str()),
                    "error end {} references an unknown declaration",
                    node.id
                );
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
    let link_pairs = link_pairs(graph_nodes)?;

    let starts: Vec<_> = graph_nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                ProcessNodeKind::Start
                    | ProcessNodeKind::TimerStart { .. }
                    | ProcessNodeKind::MessageStart { .. }
            )
        })
        .collect();
    if depth > 0 {
        ensure!(
            starts
                .iter()
                .all(|node| matches!(node.kind, ProcessNodeKind::Start)),
            "timer and message starts are root-only"
        );
    }
    ensure!(
        if depth == 0 {
            (1..=32).contains(&starts.len())
        } else {
            starts.len() == 1
        },
        "process requires eligible start events in its selected body"
    );
    ensure!(
        graph_nodes.iter().any(|node| matches!(
            node.kind,
            ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. } | ProcessNodeKind::TerminateEnd
        )),
        "process requires an end event"
    );
    for node in graph_nodes {
        let in_count = incoming.get(node.id.as_str()).map_or(0, Vec::len);
        let out_count = outgoing.get(node.id.as_str()).map_or(0, Vec::len);
        match &node.kind {
            ProcessNodeKind::LinkThrow { .. } => ensure!(
                in_count >= 1 && out_count == 0 && link_pairs.contains_key(node.id.as_str()),
                "Link Throw {} requires incoming flows and one Catch in its body",
                node.id
            ),
            ProcessNodeKind::LinkCatch { .. } => ensure!(
                in_count == 0 && out_count == 1,
                "Link Catch {} requires one outgoing flow and no incoming flow",
                node.id
            ),
            ProcessNodeKind::Start
            | ProcessNodeKind::TimerStart { .. }
            | ProcessNodeKind::MessageStart { .. } => ensure!(
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
                                | ProcessNodeKind::CallActivity(..)
                        )
                    ) || (!matches!(
                        node.kind,
                        ProcessNodeKind::BoundaryError { .. }
                            | ProcessNodeKind::BoundaryEscalation { .. }
                    ) && matches!(
                        nodes.get(attached_to_id.as_str()).map(|node| &node.kind),
                        Some(ProcessNodeKind::UserTask { .. })
                    )) || (matches!(
                        node.kind,
                        ProcessNodeKind::BoundaryTimer { .. }
                            | ProcessNodeKind::BoundaryMessage { .. }
                    ) && matches!(
                        nodes.get(attached_to_id.as_str()).map(|node| &node.kind),
                        Some(
                            ProcessNodeKind::SendTask { .. }
                                | ProcessNodeKind::ManualTask { .. }
                                | ProcessNodeKind::ReceiveTask { .. }
                                | ProcessNodeKind::ScriptTask { .. }
                        )
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
                let branches: Vec<_> = graph_flows
                    .iter()
                    .filter(|flow| flow.source_id == node.id)
                    .map(|flow| nodes[flow.target_id.as_str()])
                    .collect();
                if branches
                    .iter()
                    .any(|branch| matches!(branch.kind, ProcessNodeKind::ReceiveTask { .. }))
                    && branches
                        .iter()
                        .any(|branch| matches!(branch.kind, ProcessNodeKind::MessageCatch { .. }))
                {
                    bail!(EventGatewayProfileError {
                        node_id: node.id.clone(),
                        reason: format!(
                            "event gateway {} cannot mix ReceiveTask with MessageCatch",
                            node.id
                        )
                    });
                }
                for flow in graph_flows.iter().filter(|flow| flow.source_id == node.id) {
                    ensure!(
                        flow.condition.is_none(),
                        "event gateway {} cannot have conditions",
                        node.id
                    );
                    let branch = nodes[flow.target_id.as_str()];
                    if !matches!(
                        branch.kind,
                        ProcessNodeKind::MessageCatch { .. }
                            | ProcessNodeKind::ReceiveTask { .. }
                            | ProcessNodeKind::SignalCatch { .. }
                    ) && !matches!(&branch.kind, ProcessNodeKind::TimerCatch { timer }
                            if matches!(timer, ProcessTimerSpec::Date { .. } | ProcessTimerSpec::Duration { .. }
                                | ProcessTimerSpec::WorkingDuration { .. }))
                    {
                        bail!(EventGatewayProfileError { node_id: branch.id.clone(),
                            reason: format!("event gateway {} must branch directly to one-shot catches or ReceiveTask",
                                node.id) });
                    }
                    if incoming.get(branch.id.as_str()).map_or(0, Vec::len) != 1
                        || outgoing.get(branch.id.as_str()).map_or(0, Vec::len) != 1
                    {
                        bail!(EventGatewayProfileError {
                            node_id: branch.id.clone(),
                            reason: format!(
                                "event gateway child {} must have one incoming and outgoing flow",
                                branch.id
                            )
                        });
                    }
                    if matches!(branch.kind, ProcessNodeKind::ReceiveTask { .. }) {
                        if let Some(boundary) =
                            graph_nodes.iter().find(|candidate| match &candidate.kind {
                                ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
                                | ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
                                | ProcessNodeKind::BoundaryError { attached_to_id, .. }
                                | ProcessNodeKind::BoundaryEscalation { attached_to_id, .. } => {
                                    attached_to_id == &branch.id
                                }
                                _ => false,
                            })
                        {
                            bail!(EventGatewayProfileError { node_id: boundary.id.clone(),
                                reason: format!("event gateway target ReceiveTask {} cannot have attached boundary {}",
                                    branch.id, boundary.id) });
                        }
                    }
                }
            }
            ProcessNodeKind::End
            | ProcessNodeKind::ErrorEnd { .. }
            | ProcessNodeKind::TerminateEnd => ensure!(
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
                ensure!(
                    split || join,
                    "inclusive gateway {} must be a split or join with 2..=8 branches",
                    node.id
                );
                if split {
                    if let Some(default_id) = default_flow_id {
                        ensure!(
                            graph_flows.iter().any(|flow| flow.id == *default_id
                                && flow.source_id == node.id
                                && flow.condition.is_none()),
                            "inclusive gateway {} has invalid default flow {}",
                            node.id,
                            default_id
                        );
                    }
                    for flow in graph_flows.iter().filter(|flow| flow.source_id == node.id) {
                        if default_flow_id.as_deref() == Some(flow.id.as_str()) {
                            ensure!(
                                flow.condition.is_none(),
                                "inclusive gateway {} default flow {} cannot have a condition",
                                node.id,
                                flow.id
                            );
                        } else {
                            let condition = flow
                                .condition
                                .as_deref()
                                .filter(|condition| !condition.trim().is_empty())
                                .with_context(|| {
                                    format!(
                                        "inclusive gateway {} flow {} requires a condition",
                                        node.id, flow.id
                                    )
                                })?;
                            validate_expression(
                                condition,
                                &format!(
                                    "inclusive gateway {} flow {} condition",
                                    node.id, flow.id
                                ),
                                true,
                            )?;
                        }
                    }
                } else {
                    ensure!(
                        default_flow_id.is_none(),
                        "inclusive join {} cannot have a default flow",
                        node.id
                    );
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
                        graph_flows.iter().any(|flow| flow.id == *default_id
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
        if !matches!(
            node.kind,
            ProcessNodeKind::ExclusiveGateway { .. } | ProcessNodeKind::InclusiveGateway { .. }
        ) {
            ensure!(
                graph_flows
                    .iter()
                    .filter(|flow| flow.source_id == node.id)
                    .all(|flow| flow.condition.is_none()),
                "conditions require an exclusive or inclusive gateway"
            );
        } else if matches!(node.kind, ProcessNodeKind::InclusiveGateway { .. }) && out_count == 1 {
            ensure!(
                graph_flows
                    .iter()
                    .filter(|flow| flow.source_id == node.id)
                    .all(|flow| flow.condition.is_none()),
                "inclusive join {} cannot have an outgoing condition",
                node.id
            );
        }
    }
    let pairs = gateway_pairs(graph_nodes, graph_flows)?;
    let joins: HashMap<&str, &str> = pairs
        .iter()
        .filter_map(|(split, pair)| {
            pair.join_node_id
                .as_deref()
                .map(|join| (join, split.as_str()))
        })
        .collect();
    let mut graph_outgoing = outgoing.clone();
    let mut degree: HashMap<&str, usize> = nodes
        .keys()
        .map(|id| (*id, incoming.get(id).map_or(0, Vec::len)))
        .collect();
    for (throw_id, catch_id) in &link_pairs {
        graph_outgoing.entry(*throw_id).or_default().push(*catch_id);
        *degree.get_mut(*catch_id).context("Link Catch is absent from its body")? += 1;
    }
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
            *degree
                .get_mut(node.id.as_str())
                .expect("boundary node validated") += 1;
        }
    }
    let mut queue = VecDeque::from_iter(starts.iter().map(|start| start.id.as_str()));
    let mut order = Vec::with_capacity(nodes.len());
    if link_pairs.is_empty() {
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
    } else {
        let mut reached = HashSet::new();
        while let Some(node_id) = queue.pop_front() {
            if !reached.insert(node_id) {
                continue;
            }
            order.push(node_id);
            queue.extend(graph_outgoing.get(node_id).into_iter().flatten().copied());
        }
        ensure!(order.len() == graph_nodes.len(),
            "Link graph contains an unreachable node");
    }
    let mut can_end = HashSet::new();
    if link_pairs.is_empty() {
        for node_id in order.iter().rev() {
            if matches!(
                nodes[node_id].kind,
                ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. } | ProcessNodeKind::TerminateEnd
            ) || graph_outgoing
                .get(node_id)
                .into_iter()
                .flatten()
                .any(|target| can_end.contains(target))
            {
                can_end.insert(*node_id);
            }
        }
    } else {
        let mut previous = HashMap::<&str, Vec<&str>>::new();
        for (source, targets) in &graph_outgoing {
            for target in targets {
                previous.entry(*target).or_default().push(*source);
            }
        }
        let mut reverse = VecDeque::new();
        for node in graph_nodes {
            if matches!(node.kind,
                ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. }
                    | ProcessNodeKind::TerminateEnd)
            {
                reverse.push_back(node.id.as_str());
            }
        }
        while let Some(node_id) = reverse.pop_front() {
            if can_end.insert(node_id) {
                reverse.extend(previous.get(node_id).into_iter().flatten().copied());
            }
        }
    }
    ensure!(
        can_end.len() == nodes.len(),
        "process contains a dead-end path"
    );
    let mut states: HashMap<&str, (String, Vec<String>)> = HashMap::new();
    for start in &starts {
        states.insert(start.id.as_str(), (start.id.clone(), Vec::new()));
    }
    for node_id in &order {
        let (region, mut stack) = states
            .get(node_id)
            .cloned()
            .context("process node has no activation region")?;
        let node = nodes[node_id];
        if matches!(
            node.kind,
            ProcessNodeKind::ParallelGateway | ProcessNodeKind::InclusiveGateway { .. }
        ) {
            if outgoing.get(node_id).is_some_and(|flows| flows.len() >= 2) {
                stack.push((*node_id).to_string());
            } else {
                let split = joins
                    .get(node_id)
                    .context("gateway join lacks its paired split")?;
                ensure!(
                    stack.pop().as_deref() == Some(*split),
                    "gateway join {} has an invalid activation stack",
                    node_id
                );
            }
        }
        if matches!(
            node.kind,
            ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. }
        ) {
            ensure!(stack.is_empty(), "end event has an open gateway activation");
        }
        for target in graph_outgoing.get(node_id).into_iter().flatten() {
            if matches!(
                nodes[target].kind,
                ProcessNodeKind::End
                    | ProcessNodeKind::ErrorEnd { .. }
                    | ProcessNodeKind::TerminateEnd
            ) {
                if !matches!(nodes[target].kind, ProcessNodeKind::TerminateEnd) {
                    ensure!(
                        stack.is_empty(),
                        "boundary or main path ends inside a gateway fork"
                    );
                }
                states
                    .entry(*target)
                    .or_insert_with(|| (region.clone(), stack.clone()));
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
    validate_event_gateway_regions(graph_nodes, graph_flows, &nodes, &graph_outgoing, &order)?;
    validate_escalation_prefixes(graph_nodes, graph_flows, true)?;
    validate_diagram(diagram, graph_nodes, graph_flows, &nodes, &flow_ids)?;
    Ok(())
}

#[derive(Debug)]
pub(super) struct EventGatewayProfileError {
    pub node_id: String,
    pub reason: String,
}

impl std::fmt::Display for EventGatewayProfileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.reason)
    }
}

impl std::error::Error for EventGatewayProfileError {}

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
                ProcessNodeKind::MessageThrow { .. }
                | ProcessNodeKind::SendTask { .. }
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
                ProcessNodeKind::End
                | ProcessNodeKind::ErrorEnd { .. }
                | ProcessNodeKind::TerminateEnd => Err(self.failure(
                    incoming_flow,
                    Some(node_id),
                    ProcessEscalationPathReason::TerminalBeforeWait,
                )),
                ProcessNodeKind::SubProcess { .. } => Err(self.failure(
                    incoming_flow,
                    Some(node_id),
                    ProcessEscalationPathReason::ScopeEntry,
                )),
                ProcessNodeKind::CallActivity(..) => Err(self.failure(
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
    for gateway in graph_nodes
        .iter()
        .filter(|node| matches!(node.kind, ProcessNodeKind::EventBasedGateway))
    {
        let branches = outgoing
            .get(gateway.id.as_str())
            .context("event gateway lacks branches")?;
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
        let common = order
            .iter()
            .copied()
            .find(|node_id| branch_reach.iter().all(|reach| reach.contains(node_id)));
        if let Some(common) = common {
            ensure!(
                matches!(
                    nodes[common].kind,
                    ProcessNodeKind::End
                        | ProcessNodeKind::ErrorEnd { .. }
                        | ProcessNodeKind::TerminateEnd
                ) || matches!(&nodes[common].kind, ProcessNodeKind::ExclusiveGateway { default_flow_id: None }
                        if outgoing.get(common).map_or(0, Vec::len) == 1
                            && graph_flows.iter().filter(|flow| flow.source_id == common).all(|flow| flow.condition.is_none())),
                "event gateway {} branches merge at unsupported node {}",
                gateway.id,
                common
            );
        } else {
            ensure!(
                branch_reach.iter().any(|reach| reach
                    .iter()
                    .any(|id| matches!(nodes[*id].kind, ProcessNodeKind::TerminateEnd))),
                "event gateway {} has no common exclusive merge or terminal escape",
                gateway.id
            );
        }
        let mut visited_regions = HashSet::new();
        for branch in branches {
            let mut reached = HashSet::new();
            let mut queue = VecDeque::from([*branch]);
            while let Some(current) = queue.pop_front() {
                if common == Some(current) || !reached.insert(current) {
                    continue;
                }
                if matches!(nodes[current].kind, ProcessNodeKind::TerminateEnd) {
                    continue;
                }
                if matches!(
                    nodes[current].kind,
                    ProcessNodeKind::End | ProcessNodeKind::ErrorEnd { .. }
                ) {
                    ensure!(
                        common.is_none(),
                        "event gateway {} branch ends before common merge {}",
                        gateway.id,
                        common.unwrap_or_default()
                    );
                    continue;
                }
                ensure!(
                    !matches!(nodes[current].kind, ProcessNodeKind::EventBasedGateway),
                    "event gateway {} has a nested event race",
                    gateway.id
                );
                ensure!(
                    visited_regions.insert(current),
                    "event gateway {} branches share node {} before merge",
                    gateway.id,
                    current
                );
                queue.extend(outgoing.get(current).into_iter().flatten().copied());
            }
        }
    }
    Ok(())
}

fn validate_mapping(
    mapping: &std::collections::BTreeMap<String, String>,
    script_profile: bool,
) -> Result<()> {
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
        diagram.shapes.len() <= graph_nodes.len() && diagram.edges.len() <= graph_flows.len(),
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

fn validate_activity_io<'a>(
    node: &'a ProcessNode,
    modeling: Option<&ProcessBodyModeling>,
    variables: &BTreeMap<String, serde_json::Value>,
    all_ids: &mut HashSet<&'a str>,
    complete: bool,
) -> Result<()> {
    let Some(io) = &node.activity_io else {
        return Ok(());
    };
    ensure!(
        node.repeat.is_some() || io.coordinator_output.is_none(),
        "coordinator output requires a repeated activity",
    );
    ensure!(
        io.data_inputs.len() <= 16 && io.data_outputs.len() <= 16,
        "activity {} IO exceeds sixteen inputs or outputs",
        node.id
    );
    ensure!(
        matches!(
            node.kind,
            ProcessNodeKind::UserTask { .. }
                | ProcessNodeKind::ScriptTask { .. }
                | ProcessNodeKind::ServiceTask { .. }
                | ProcessNodeKind::ManualTask { .. }
                | ProcessNodeKind::SendTask { .. }
                | ProcessNodeKind::ReceiveTask { .. }
                | ProcessNodeKind::SubProcess { .. }
                | ProcessNodeKind::CallActivity(..)
        ),
        "activity IO on {} requires an activity",
        node.id
    );
    let mut inputs = HashSet::new();
    for input in &io.data_inputs {
        ensure!(
            valid_id(&input.id) && all_ids.insert(input.id.as_str()),
            "invalid or duplicate activity input ID: {}",
            input.id
        );
        ensure!(
            input
                .name
                .as_deref()
                .is_none_or(|name| name.len() <= 256 && !name.chars().any(char::is_control)),
            "invalid activity input name"
        );
        inputs.insert(input.id.as_str());
    }
    let mut outputs = HashSet::new();
    for output in &io.data_outputs {
        ensure!(
            valid_id(&output.id) && all_ids.insert(output.id.as_str()),
            "invalid or duplicate activity output ID: {}",
            output.id
        );
        ensure!(
            output
                .name
                .as_deref()
                .is_none_or(|name| name.len() <= 256 && !name.chars().any(char::is_control)),
            "invalid activity output name"
        );
        validate_expression(
            &output.value_expression,
            "activity output value expression",
            complete,
        )?;
        outputs.insert(output.id.as_str());
    }
    ensure!(
        valid_id(&io.input_set_id)
            && all_ids.insert(io.input_set_id.as_str())
            && valid_id(&io.output_set_id)
            && all_ids.insert(io.output_set_id.as_str()),
        "invalid or duplicate activity IO set ID"
    );
    if complete {
        ensure!(
            io.input_set.len() == inputs.len()
                && io.input_set.iter().all(|id| inputs.contains(id.as_str()))
                && io.input_set.iter().collect::<HashSet<_>>().len() == io.input_set.len()
                && io.output_set.len() == outputs.len()
                && io.output_set.iter().all(|id| outputs.contains(id.as_str()))
                && io.output_set.iter().collect::<HashSet<_>>().len() == io.output_set.len(),
            "activity {} IO sets must contain each data item exactly once",
            node.id
        );
    }
    let references = modeling.map(|modeling| &modeling.data_object_references);
    let mut input_targets = HashSet::new();
    for association in &io.input_associations {
        match association {
            tentaflow_protocol::processes::ProcessInputAssociation::DirectRef {
                id,
                source_object_ref_id,
                target_input_id,
            } => {
                ensure!(
                    valid_id(id) && all_ids.insert(id.as_str()),
                    "invalid or duplicate activity input association ID: {id}"
                );
                if complete {
                    ensure!(
                        references.is_some_and(|references| references.iter().any(|reference| {
                            reference.id == *source_object_ref_id
                                && reference
                                    .variable_binding_key
                                    .as_ref()
                                    .is_some_and(|key| variables.contains_key(key))
                        })),
                        "activity {} input association requires a bound same-body object",
                        node.id
                    );
                    ensure!(
                        inputs.contains(target_input_id.as_str())
                            && input_targets.insert(target_input_id.as_str()),
                        "activity {} input association has a missing or duplicate target",
                        node.id
                    );
                }
            }
            tentaflow_protocol::processes::ProcessInputAssociation::CelAssignment {
                id,
                from_expression,
                target_input_id,
            } => {
                ensure!(
                    valid_id(id) && all_ids.insert(id.as_str()),
                    "invalid or duplicate activity input association ID: {id}"
                );
                validate_expression(from_expression, "activity input assignment", complete)?;
                if complete {
                    ensure!(
                        inputs.contains(target_input_id.as_str())
                            && input_targets.insert(target_input_id.as_str()),
                        "activity {} input association has a missing or duplicate target",
                        node.id
                    );
                }
            }
        }
    }
    let mut output_sources = HashSet::new();
    let mut output_targets = HashSet::new();
    let mut output_aliases = HashSet::new();
    let legacy_output_mapping = match &node.kind {
        ProcessNodeKind::UserTask { output_mapping, .. }
        | ProcessNodeKind::ScriptTask { output_mapping, .. }
        | ProcessNodeKind::ServiceTask { output_mapping, .. }
        | ProcessNodeKind::ReceiveTask { output_mapping, .. }
        | ProcessNodeKind::SubProcess { output_mapping, .. } => Some(output_mapping),
        ProcessNodeKind::CallActivity(call) => Some(&call.output_mapping),
        _ => None,
    };
    let repeat_output_alias = node.repeat.as_ref().map(|repeat| match repeat {
        ProcessRepeatSpec::MultiInstance { output_collection_variable, .. }
        | ProcessRepeatSpec::StructuredLoop { output_collection_variable, .. } =>
            output_collection_variable.as_str(),
    });
    for association in &io.output_associations {
        ensure!(
            valid_id(&association.id) && all_ids.insert(association.id.as_str()),
            "invalid or duplicate activity output association ID: {}",
            association.id
        );
        if complete {
            ensure!(
                outputs.contains(association.source_output_id.as_str())
                    && output_sources.insert(association.source_output_id.as_str()),
                "activity {} output association has a missing or duplicate source",
                node.id
            );
            ensure!(
                references.is_some_and(|references| references.iter().any(|reference| reference
                    .id
                    == association.target_object_ref_id
                    && reference
                        .variable_binding_key
                        .as_ref()
                        .is_some_and(|key| variables.contains_key(key)))),
                "activity {} output association requires a bound same-body object",
                node.id
            );
            ensure!(
                output_targets.insert(association.target_object_ref_id.as_str()),
                "activity {} writes one data object more than once",
                node.id
            );
            let binding_key = references
                .and_then(|references| references.iter().find(|reference|
                    reference.id == association.target_object_ref_id))
                .and_then(|reference| reference.variable_binding_key.as_deref())
                .context("activity output association lacks its same-body binding")?;
            ensure!(output_aliases.insert(binding_key)
                && legacy_output_mapping.is_none_or(|mapping| !mapping.contains_key(binding_key))
                && repeat_output_alias != Some(binding_key),
                "activity {} writes one variable through conflicting output owners",
                node.id);
        }
    }
    if complete {
        ensure!(
            input_targets.len() == inputs.len() && output_sources.len() == outputs.len(),
            "activity {} requires one association per IO item",
            node.id
        );
        if !io.input_set.is_empty() {
            if let ProcessNodeKind::ServiceTask { input_mapping, .. } = &node.kind {
                let payload = input_mapping.get("payload").with_context(|| format!(
                    "service activity {} with authored inputs requires an explicit payload mapping",
                    node.id
                ))?;
                ensure!(
                    expr::references_variable(payload, "inputs")?,
                    "service activity {} payload mapping must reference the inputs namespace",
                    node.id
                );
            }
        }
    }
    if let Some(coordinator) = &io.coordinator_output {
        ensure!(coordinator.data_outputs.len() <= 16,
            "activity {} coordinator IO exceeds sixteen outputs", node.id);
        let mut declared = HashSet::new();
        for output in &coordinator.data_outputs {
            ensure!(valid_id(&output.id) && all_ids.insert(output.id.as_str()),
                "invalid or duplicate coordinator output ID: {}", output.id);
            ensure!(output.name.as_deref().is_none_or(|name|
                name.len() <= 256 && !name.chars().any(char::is_control)),
                "invalid coordinator output name");
            validate_expression(&output.value_expression,
                "coordinator output value expression", complete)?;
            declared.insert(output.id.as_str());
        }
        ensure!(valid_id(&coordinator.output_set_id)
            && all_ids.insert(coordinator.output_set_id.as_str()),
            "invalid or duplicate coordinator output set ID");
        if complete {
            ensure!(coordinator.output_set.len() == declared.len()
                && coordinator.output_set.iter().all(|id| declared.contains(id.as_str()))
                && coordinator.output_set.iter().collect::<HashSet<_>>().len()
                    == coordinator.output_set.len(),
                "coordinator output set must contain every output once");
        }
        let mut sources = HashSet::new();
        let mut targets = HashSet::new();
        let mut aliases = HashSet::new();
        for association in &coordinator.output_associations {
            ensure!(valid_id(&association.id) && all_ids.insert(association.id.as_str()),
                "invalid or duplicate coordinator output association ID: {}", association.id);
            if complete {
                ensure!(declared.contains(association.source_output_id.as_str())
                    && sources.insert(association.source_output_id.as_str()),
                    "coordinator output association has a missing or duplicate source");
                ensure!(targets.insert(association.target_object_ref_id.as_str()),
                    "coordinator output writes one data object more than once");
                let binding_key = references
                    .and_then(|references| references.iter().find(|reference|
                        reference.id == association.target_object_ref_id))
                    .and_then(|reference| reference.variable_binding_key.as_deref())
                    .filter(|key| variables.contains_key(*key))
                    .context("coordinator output requires a bound same-body object")?;
                ensure!(aliases.insert(binding_key)
                    && !output_aliases.contains(binding_key)
                    && legacy_output_mapping.is_none_or(|mapping| !mapping.contains_key(binding_key))
                    && repeat_output_alias != Some(binding_key),
                    "coordinator output conflicts with another parent output owner");
            }
        }
        if complete {
            ensure!(sources.len() == declared.len(),
                "coordinator output requires one association per output");
        }
    }
    Ok(())
}

fn validate_modeling<'a>(
    modeling: Option<&'a ProcessBodyModeling>,
    diagram: &'a ProcessDiagram,
    graph_nodes: &[ProcessNode],
    variables: &BTreeMap<String, serde_json::Value>,
    data_stores: &HashSet<&str>,
    all_ids: &mut HashSet<&'a str>,
) -> Result<()> {
    let Some(modeling) = modeling else {
        ensure!(
            diagram.modeling_shapes.is_empty() && diagram.modeling_edges.is_empty(),
            "modeling DI requires typed body modeling"
        );
        return Ok(());
    };
    let node_ids: HashSet<_> = graph_nodes.iter().map(|node| node.id.as_str()).collect();
    let mut shape_elements = HashSet::new();
    fn lanes<'a>(
        sets: &'a [tentaflow_protocol::processes::ProcessLaneSet],
        node_ids: &HashSet<&str>,
        shape_elements: &mut HashSet<&'a str>,
        all_ids: &mut HashSet<&'a str>,
        depth: usize,
    ) -> Result<()> {
        ensure!(depth <= 3, "lane nesting exceeds three levels");
        for set in sets {
            ensure!(
                valid_id(&set.id) && all_ids.insert(&set.id),
                "invalid or duplicate lane set ID: {}",
                set.id
            );
            for lane in &set.lanes {
                ensure!(
                    valid_id(&lane.id) && all_ids.insert(&lane.id),
                    "invalid or duplicate lane ID: {}",
                    lane.id
                );
                ensure!(lane.name.as_deref().is_none_or(|name| name.len() <= 256
                    && !name.chars().any(char::is_control)), "invalid lane name");
                ensure!(
                    shape_elements.insert(lane.id.as_str()),
                    "duplicate lane element"
                );
                let mut refs = HashSet::new();
                for reference in &lane.flow_node_refs {
                    ensure!(
                        node_ids.contains(reference.as_str()) && refs.insert(reference.as_str()),
                        "lane {} has a missing or duplicate flow node reference",
                        lane.id
                    );
                }
                lanes(
                    &lane.child_lane_sets,
                    node_ids,
                    shape_elements,
                    all_ids,
                    depth + 1,
                )?;
            }
        }
        Ok(())
    }
    lanes(
        &modeling.lane_sets,
        &node_ids,
        &mut shape_elements,
        all_ids,
        0,
    )?;
    let mut objects = HashSet::new();
    for object in &modeling.data_objects {
        ensure!(
            valid_id(&object.id) && all_ids.insert(&object.id),
            "invalid or duplicate data object ID: {}",
            object.id
        );
        ensure!(
            object
                .name
                .as_deref()
                .is_none_or(|name| name.len() <= 256 && !name.chars().any(char::is_control)),
            "invalid data object name"
        );
        objects.insert(object.id.as_str());
    }
    let mut references = HashSet::new();
    for reference in &modeling.data_object_references {
        ensure!(
            valid_id(&reference.id) && all_ids.insert(&reference.id),
            "invalid or duplicate data object reference ID: {}",
            reference.id
        );
        ensure!(
            objects.contains(reference.data_object_ref.as_str()),
            "data object reference {} points outside its body",
            reference.id
        );
        ensure!(
            reference
                .name
                .as_deref()
                .is_none_or(|name| name.len() <= 256 && !name.chars().any(char::is_control)),
            "invalid data object reference name"
        );
        if let Some(key) = &reference.variable_binding_key {
            ensure!(
                variables.contains_key(key),
                "data object reference {} has no same-body variable binding",
                reference.id
            );
        }
        references.insert(reference.id.as_str());
        shape_elements.insert(reference.id.as_str());
    }
    for reference in &modeling.data_store_references {
        ensure!(
            valid_id(&reference.id) && all_ids.insert(&reference.id),
            "invalid or duplicate data store reference ID: {}",
            reference.id
        );
        ensure!(
            data_stores.contains(reference.data_store_ref.as_str()),
            "data store reference {} points outside this document",
            reference.id
        );
        ensure!(
            reference.name.as_deref().is_none_or(|name| name.len() <= 256
                && !name.chars().any(char::is_control)),
            "invalid data store reference name"
        );
        shape_elements.insert(reference.id.as_str());
    }
    for annotation in &modeling.text_annotations {
        ensure!(
            valid_id(&annotation.id) && all_ids.insert(&annotation.id),
            "invalid or duplicate annotation ID: {}",
            annotation.id
        );
        ensure!(
            annotation.text.len() <= 4096
                && !annotation
                    .text
                    .chars()
                    .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\n' | '\r')),
            "invalid annotation text"
        );
        shape_elements.insert(annotation.id.as_str());
    }
    let mut association_ids = HashSet::new();
    for association in &modeling.associations {
        ensure!(
            valid_id(&association.id) && all_ids.insert(&association.id),
            "invalid or duplicate association ID: {}",
            association.id
        );
        ensure!(
            (node_ids.contains(association.source_ref.as_str())
                || shape_elements.contains(association.source_ref.as_str()))
                && (node_ids.contains(association.target_ref.as_str())
                    || shape_elements.contains(association.target_ref.as_str())),
            "association {} references an element outside its body",
            association.id
        );
        association_ids.insert(association.id.as_str());
    }
    let mut diagram_elements = HashSet::new();
    for shape in &diagram.modeling_shapes {
        ensure!(
            valid_id(&shape.di_id)
                && all_ids.insert(&shape.di_id)
                && shape_elements.contains(shape.element_id.as_str())
                && diagram_elements.insert(shape.element_id.as_str()),
            "invalid or duplicate modeling shape: {}",
            shape.di_id
        );
        ensure!(
            [shape.x, shape.y, shape.width, shape.height]
                .iter()
                .all(|value| value.is_finite() && value.abs() <= MAX_DI_COORDINATE)
                && shape.width > 0.0
                && shape.height > 0.0,
            "invalid modeling shape bounds"
        );
    }
    diagram_elements.clear();
    for edge in &diagram.modeling_edges {
        ensure!(
            valid_id(&edge.di_id)
                && all_ids.insert(&edge.di_id)
                && association_ids.contains(edge.element_id.as_str())
                && diagram_elements.insert(edge.element_id.as_str()),
            "invalid or duplicate modeling edge: {}",
            edge.di_id
        );
        ensure!(
            (2..=64).contains(&edge.waypoints.len())
                && edge.waypoints.iter().all(|point| point.x.is_finite()
                    && point.y.is_finite()
                    && point.x.abs() <= MAX_DI_COORDINATE
                    && point.y.abs() <= MAX_DI_COORDINATE),
            "invalid modeling edge waypoints"
        );
    }
    Ok(())
}

fn validate_collaboration<'a>(
    model: &'a ProcessModel,
    all_ids: &mut HashSet<&'a str>,
) -> Result<()> {
    let Some(collaboration) = &model.collaboration else {
        return Ok(());
    };
    ensure!(
        valid_id(&collaboration.id) && all_ids.insert(&collaboration.id),
        "invalid or duplicate collaboration ID"
    );
    ensure!(
        collaboration
            .name
            .as_deref()
            .is_none_or(|name| name.len() <= 256 && !name.chars().any(char::is_control)),
        "invalid collaboration name"
    );
    ensure!(
        collaboration.diagram.shapes.is_empty() && collaboration.diagram.edges.is_empty(),
        "collaboration diagram requires typed modeling DI"
    );
    let namespace = model
        .target_namespace
        .as_deref()
        .unwrap_or("https://tentaflow.app/bpmn/1");
    let mut participants = HashSet::new();
    for participant in &collaboration.participants {
        ensure!(
            valid_id(&participant.id) && all_ids.insert(&participant.id),
            "invalid or duplicate participant ID: {}",
            participant.id
        );
        ensure!(
            participant
                .name
                .as_deref()
                .is_none_or(|name| name.len() <= 256 && !name.chars().any(char::is_control)),
            "invalid participant name"
        );
        if let Some(reference) = &participant.process_ref {
            ensure!(
                reference.namespace_uri == namespace
                    && (reference.process_id == model.process_id
                        || model
                            .additional_processes
                            .iter()
                            .any(|process| process.process_id == reference.process_id)),
                "participant {} references a process outside this document",
                participant.id
            );
        }
        participants.insert(participant.id.as_str());
    }
    let nodes = all_nodes(model);
    let node_ids: HashSet<_> = nodes.iter().map(|node| node.id.as_str()).collect();
    let message_ids: HashSet<_> = model
        .messages
        .iter()
        .map(|message| message.message_id.as_str())
        .collect();
    let mut message_flows = HashSet::new();
    for flow in &collaboration.message_flows {
        ensure!(
            valid_id(&flow.id) && all_ids.insert(&flow.id),
            "invalid or duplicate message flow ID: {}",
            flow.id
        );
        ensure!(
            flow.source_ref != flow.target_ref
                && (participants.contains(flow.source_ref.as_str())
                    || node_ids.contains(flow.source_ref.as_str()))
                && (participants.contains(flow.target_ref.as_str())
                    || node_ids.contains(flow.target_ref.as_str())),
            "message flow {} references an unknown endpoint",
            flow.id
        );
        ensure!(
            flow.message_ref
                .as_deref()
                .is_none_or(|id| message_ids.contains(id)),
            "message flow {} references an unknown message",
            flow.id
        );
        message_flows.insert(flow.id.as_str());
    }
    let mut seen_shapes = HashSet::new();
    for shape in &collaboration.diagram.modeling_shapes {
        ensure!(
            valid_id(&shape.di_id)
                && all_ids.insert(&shape.di_id)
                && participants.contains(shape.element_id.as_str())
                && seen_shapes.insert(shape.element_id.as_str()),
            "invalid or duplicate participant DI shape"
        );
        ensure!(
            [shape.x, shape.y, shape.width, shape.height]
                .iter()
                .all(|value| value.is_finite() && value.abs() <= MAX_DI_COORDINATE)
                && shape.width > 0.0
                && shape.height > 0.0,
            "invalid participant DI bounds"
        );
    }
    let mut seen_edges = HashSet::new();
    for edge in &collaboration.diagram.modeling_edges {
        ensure!(
            valid_id(&edge.di_id)
                && all_ids.insert(&edge.di_id)
                && message_flows.contains(edge.element_id.as_str())
                && seen_edges.insert(edge.element_id.as_str()),
            "invalid or duplicate message-flow DI edge"
        );
        ensure!(
            (2..=64).contains(&edge.waypoints.len())
                && edge.waypoints.iter().all(|point| point.x.is_finite()
                    && point.y.is_finite()
                    && point.x.abs() <= MAX_DI_COORDINATE
                    && point.y.abs() <= MAX_DI_COORDINATE),
            "invalid message-flow DI waypoints"
        );
    }
    Ok(())
}

/// Finds structured gateway regions and their exact join or terminal exits.
pub fn gateway_pairs(
    nodes: &[ProcessNode],
    flows: &[ProcessSequenceFlow],
) -> Result<HashMap<String, GatewayPair>> {
    let link_pairs = link_pairs(nodes)?;
    let kind = |node: &ProcessNode| match node.kind {
        ProcessNodeKind::ParallelGateway => Some(GatewayKind::Parallel),
        ProcessNodeKind::InclusiveGateway { .. } => Some(GatewayKind::Inclusive),
        _ => None,
    };
    let outgoing = |id: &str| {
        flows
            .iter()
            .filter(|flow| flow.source_id == id)
            .collect::<Vec<_>>()
    };
    let incoming = |id: &str| {
        flows
            .iter()
            .filter(|flow| flow.target_id == id)
            .collect::<Vec<_>>()
    };
    let by_id = nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<HashMap<_, _>>();
    let joins: Vec<_> = nodes
        .iter()
        .filter(|node| kind(node).is_some() && incoming(&node.id).len() >= 2)
        .collect();
    let splits: Vec<_> = nodes
        .iter()
        .filter(|node| kind(node).is_some() && outgoing(&node.id).len() >= 2)
        .collect();
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
                if !seen.insert(node_id) {
                    continue;
                }
                if let Some(catch_id) = link_pairs.get(node_id) {
                    pending.push((*catch_id, arrival_id));
                    continue;
                }
                let next = outgoing(node_id);
                if next.is_empty() {
                    return None;
                }
                pending.extend(
                    next.iter()
                        .map(|edge| (edge.target_id.as_str(), edge.id.as_str())),
                );
            }
            if arrivals.len() > 1 || arrivals.is_empty() && terminals.is_empty() {
                return None;
            }
            branch_paths.push(seen);
            branches.insert(
                branch.id.clone(),
                GatewayBranchExits {
                    join_incoming_edge_id: arrivals.into_iter().next(),
                    terminate_end_node_ids: terminals,
                },
            );
        }
        if branch_paths.iter().enumerate().any(|(index, path)| {
            branch_paths
                .iter()
                .skip(index + 1)
                .any(|other| !path.is_disjoint(other))
        }) {
            return None;
        }
        if let Some(join) = join {
            let arrivals = branches
                .values()
                .filter_map(|exit| exit.join_incoming_edge_id.as_ref())
                .collect::<Vec<_>>();
            if arrivals.len() != incoming(&join.id).len()
                || arrivals.iter().collect::<HashSet<_>>().len() != arrivals.len()
            {
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
            ensure!(
                used_joins.insert(join_id.clone()),
                "gateway join {} is paired twice",
                join_id
            );
        }
        pairs.insert(
            split.id.clone(),
            GatewayPair {
                kind: kind(split).expect("split kind checked"),
                split_node_id: split.id.clone(),
                join_node_id,
                branches,
            },
        );
    }
    ensure!(
        used_joins.len() == joins.len(),
        "unpaired structured gateway join"
    );
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::processes::{ProcessLinkEventDefinition, ProcessMultiInstanceMode};

    #[test]
    fn coordinator_and_ordinal_outputs_cannot_claim_the_same_parent_binding() {
        use tentaflow_protocol::processes::{ProcessActivityIo, ProcessCoordinatorOutputIo,
            ProcessDataObjectReference, ProcessIoDataOutput, ProcessOutputAssociation};
        let output = |id: &str| ProcessIoDataOutput {
            id: id.into(), name: None, value_expression: "outputs".into(),
        };
        let association = |id: &str, source: &str, target: &str| ProcessOutputAssociation {
            id: id.into(), source_output_id: source.into(),
            target_object_ref_id: target.into(),
        };
        let mut modeling = ProcessBodyModeling {
            data_object_references: vec![
                ProcessDataObjectReference { id: "Ref_Ordinal".into(), name: None,
                    data_object_ref: "Object_Ordinal".into(),
                    variable_binding_key: Some("shared".into()) },
                ProcessDataObjectReference { id: "Ref_Coordinator".into(), name: None,
                    data_object_ref: "Object_Coordinator".into(),
                    variable_binding_key: Some("shared".into()) },
            ],
            ..ProcessBodyModeling::default()
        };
        let node = ProcessNode {
            id: "Review".into(), name: "Review the collection".into(),
            kind: ProcessNodeKind::UserTask { assignee_user_id: None,
                output_mapping: BTreeMap::new() },
            repeat: Some(ProcessRepeatSpec::MultiInstance {
                mode: ProcessMultiInstanceMode::Sequential,
                input: ProcessMultiInstanceInput::Cardinality { count: 1 },
                output_collection_variable: "results".into(),
            }),
            activity_io: Some(ProcessActivityIo {
                data_inputs: Vec::new(), data_outputs: vec![output("OrdinalOutput")],
                input_set_id: "InputSet_Review".into(), input_set: Vec::new(),
                output_set_id: "OutputSet_Ordinal".into(),
                output_set: vec!["OrdinalOutput".into()],
                input_associations: Vec::new(),
                output_associations: vec![association("Assoc_Ordinal", "OrdinalOutput", "Ref_Ordinal")],
                coordinator_output: Some(ProcessCoordinatorOutputIo {
                    data_outputs: vec![output("CoordinatorOutput")],
                    output_set_id: "OutputSet_Coordinator".into(),
                    output_set: vec!["CoordinatorOutput".into()],
                    output_associations: vec![association("Assoc_Coordinator",
                        "CoordinatorOutput", "Ref_Coordinator")],
                }),
            }),
        };
        let variables = BTreeMap::from([
            ("shared".into(), serde_json::Value::Null),
            ("separate".into(), serde_json::Value::Null),
            ("results".into(), serde_json::json!([])),
        ]);
        let mut ids = HashSet::new();
        assert!(validate_activity_io(&node, Some(&modeling), &variables, &mut ids, true)
            .unwrap_err().to_string().contains("conflicts with another parent output owner"));
        modeling.data_object_references[1].variable_binding_key = Some("separate".into());
        let mut ids = HashSet::new();
        validate_activity_io(&node, Some(&modeling), &variables, &mut ids, true).unwrap();
    }

    #[test]
    fn link_cycle_through_a_durable_wait_retains_the_selected_body_region() {
        let mut model = starter_model();
        model.variables.insert("loop".into(), serde_json::json!(false));
        model.nodes.push(ProcessNode {
            activity_io: None,
            repeat: None,
            id: "Choice_1".into(),
            name: "Continue?".into(),
            kind: ProcessNodeKind::ExclusiveGateway {
                default_flow_id: Some("Flow_Exit".into()),
            },
        });
        model.nodes.push(ProcessNode {
            activity_io: None,
            repeat: None,
            id: "Review_1".into(),
            name: "Review".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            },
        });
        model.nodes.push(ProcessNode {
            activity_io: None,
            repeat: None,
            id: "LinkThrow_1".into(),
            name: "Again".into(),
            kind: ProcessNodeKind::LinkThrow {
                definition: ProcessLinkEventDefinition {
                    id: "LinkDefinition_Throw".into(),
                    name: "repeat_review".into(),
                    source_refs: Vec::new(),
                    target_ref: Some("LinkDefinition_Catch".into()),
                },
            },
        });
        model.nodes.push(ProcessNode {
            activity_io: None,
            repeat: None,
            id: "LinkCatch_1".into(),
            name: "Resume".into(),
            kind: ProcessNodeKind::LinkCatch {
                definition: ProcessLinkEventDefinition {
                    id: "LinkDefinition_Catch".into(),
                    name: "repeat_review".into(),
                    source_refs: vec!["LinkDefinition_Throw".into()],
                    target_ref: None,
                },
            },
        });
        model.sequence_flows[0].target_id = "Choice_1".into();
        for (id, source_id, target_id, condition) in [
            ("Flow_Review", "Choice_1", "Review_1", Some("vars.loop")),
            ("Flow_Exit", "Choice_1", "End_1", None),
            ("Flow_Throw", "Review_1", "LinkThrow_1", None),
            ("Flow_Resume", "LinkCatch_1", "Choice_1", None),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                call_start_node_id: None,
                id: id.into(),
                source_id: source_id.into(),
                target_id: target_id.into(),
                condition: condition.map(str::to_string),
            });
        }
        validate_model(&model).expect("Link cycle with a factual wait is valid");
        if let ProcessNodeKind::LinkCatch { definition } = &mut model.nodes[5].kind {
            definition.name = "different_destination".into();
        }
        assert!(validate_model(&model).is_err());
    }

    #[test]
    fn data_store_reference_requires_a_document_declaration_and_unique_ids() {
        use tentaflow_protocol::processes::{ProcessDataStore, ProcessDataStoreReference, ProcessModelingShape};
        let mut model = starter_model();
        model.data_stores = vec![ProcessDataStore {
            id: "Store_1".into(),
            name: Some("Retained case metadata".into()),
            capacity: Some(12),
            is_unlimited: Some(false),
        }];
        model.modeling = Some(ProcessBodyModeling {
            data_store_references: vec![ProcessDataStoreReference {
                id: "StoreRef_1".into(),
                name: Some(String::new()),
                data_store_ref: "Store_1".into(),
            }],
            ..Default::default()
        });
        model.diagram.modeling_shapes.push(ProcessModelingShape {
            di_id: "Shape_StoreRef".into(), element_id: "StoreRef_1".into(),
            x: 320.0, y: 80.0, width: 160.0, height: 90.0,
        });
        validate_model(&model).unwrap();
        model.modeling.as_mut().unwrap().data_store_references[0].data_store_ref = "Missing_Store".into();
        assert!(validate_model(&model).unwrap_err().to_string().contains("points outside this document"));
        model.modeling.as_mut().unwrap().data_store_references[0].data_store_ref = "Store_1".into();
        model.data_stores[0].capacity = Some(MAX_DATA_STORE_CAPACITY);
        validate_model(&model).unwrap();
        model.data_stores[0].capacity = Some(MAX_DATA_STORE_CAPACITY + 1);
        assert!(validate_model(&model).unwrap_err().to_string().contains("invalid data store capacity"));
        model.data_stores[0].capacity = Some(12);
        model.data_stores.push(model.data_stores[0].clone());
        assert!(validate_model(&model).unwrap_err().to_string().contains("duplicate data store ID"));
        model.data_stores.pop();
        model.data_stores[0].id = "Start_1".into();
        assert!(validate_model(&model).unwrap_err().to_string().contains("duplicate BPMN ID"));
    }

    #[test]
    fn annotation_accepts_xml_text_lines_but_rejects_forbidden_controls() {
        let mut model = starter_model();
        model.modeling = Some(ProcessBodyModeling {
            text_annotations: vec![tentaflow_protocol::processes::ProcessTextAnnotation {
                id: "Note_1".into(),
                text: "First line\n\tSecond line\rThird line".into(),
            }],
            ..Default::default()
        });
        validate_model(&model).unwrap();
        model.modeling.as_mut().unwrap().text_annotations[0].text = "Forbidden\0control".into();
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("invalid annotation text"));
    }

    fn escalation_model() -> ProcessModel {
        use tentaflow_protocol::processes::{ActivityVerification, ProcessEscalationDeclaration};
        let mut model = starter_model();
        model.escalations.push(ProcessEscalationDeclaration {
            escalation_id: "Escalation_1".into(),
            name: "Human review".into(),
            escalation_code: "NEEDS.HUMAN".into(),
        });
        model.nodes.extend([
            ProcessNode {
                activity_io: None,
                repeat: None,
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
            ProcessNode {
                activity_io: None,
                repeat: None,
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
            ProcessNode {
                activity_io: None,
                repeat: None,
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
                call_start_node_id: None,
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
        model
            .nodes
            .iter_mut()
            .find(|node| node.id == "End_1")
            .unwrap()
            .kind = ProcessNodeKind::TerminateEnd;
        assert_eq!(
            validate_model(&model)
                .unwrap_err()
                .downcast_ref::<EscalationPathError>()
                .unwrap()
                .reason,
            ProcessEscalationPathReason::TerminalBeforeWait
        );
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
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Split_1".into(),
                name: "Split".into(),
                kind: ProcessNodeKind::InclusiveGateway {
                    default_flow_id: Some("To_Sync".into()),
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Join_1".into(),
                name: "Join".into(),
                kind: ProcessNodeKind::InclusiveGateway {
                    default_flow_id: None,
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
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
                call_start_node_id: None,
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
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Split".into(),
                name: "Select".into(),
                kind: ProcessNodeKind::InclusiveGateway {
                    default_flow_id: Some("To_C".into()),
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Join".into(),
                name: "Synchronize".into(),
                kind: ProcessNodeKind::InclusiveGateway {
                    default_flow_id: None,
                },
            },
        ]);
        for id in ["A", "B", "C"] {
            model.nodes.push(ProcessNode {
                activity_io: None,
                repeat: None,
                id: id.into(),
                name: id.into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: Default::default(),
                },
            });
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
                call_start_node_id: None,
                id: id.into(),
                source_id: source.into(),
                target_id: target.into(),
                condition: condition.map(str::to_string),
            });
        }
        validate_model(&model).unwrap();
        let pair = gateway_pairs(&model.nodes, &model.sequence_flows)
            .unwrap()
            .remove("Split")
            .unwrap();
        assert_eq!(pair.kind, GatewayKind::Inclusive);
        assert_eq!(pair.join_node_id.as_deref(), Some("Join"));
        for (branch, arrival) in [("To_A", "From_A"), ("To_B", "From_B"), ("To_C", "From_C")] {
            assert_eq!(
                pair.branches[branch].join_incoming_edge_id.as_deref(),
                Some(arrival)
            );
            assert!(pair.branches[branch].terminate_end_node_ids.is_empty());
        }

        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "To_B")
            .unwrap()
            .condition = None;
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("To_B"));
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "To_B")
            .unwrap()
            .condition = Some("vars.b == true".into());
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "To_C")
            .unwrap()
            .condition = Some("true".into());
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("default flow"));
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "To_C")
            .unwrap()
            .condition = None;
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "From_A")
            .unwrap()
            .target_id = "B".into();
        assert!(
            validate_model(&model).is_err(),
            "one branch cannot enter another selected branch"
        );
    }

    #[test]
    fn parallel_pair_keeps_nine_branches_without_the_inclusive_limit() {
        let mut model = starter_model();
        model.nodes.extend([
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Split".into(),
                name: String::new(),
                kind: ProcessNodeKind::ParallelGateway,
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Join".into(),
                name: String::new(),
                kind: ProcessNodeKind::ParallelGateway,
            },
        ]);
        model.sequence_flows[0].target_id = "Split".into();
        for branch in 0..9 {
            let id = format!("Branch_{branch}");
            model.nodes.push(ProcessNode {
                activity_io: None,
                repeat: None,
                id: id.clone(),
                name: id.clone(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: Default::default(),
                },
            });
            model.sequence_flows.push(ProcessSequenceFlow {
                call_start_node_id: None,
                id: format!("To_{branch}"),
                source_id: "Split".into(),
                target_id: id.clone(),
                condition: None,
            });
            model.sequence_flows.push(ProcessSequenceFlow {
                call_start_node_id: None,
                id: format!("From_{branch}"),
                source_id: id,
                target_id: "Join".into(),
                condition: None,
            });
        }
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "After_Join".into(),
            source_id: "Join".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&model).unwrap();
        assert_eq!(
            gateway_pairs(&model.nodes, &model.sequence_flows).unwrap()["Split"]
                .branches
                .len(),
            9
        );
        model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Split")
            .unwrap()
            .kind = ProcessNodeKind::InclusiveGateway {
            default_flow_id: None,
        };
        model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Join")
            .unwrap()
            .kind = ProcessNodeKind::InclusiveGateway {
            default_flow_id: None,
        };
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("Split"));
    }

    #[test]
    fn terminate_end_closes_open_gateway_branches_without_inventing_a_join() {
        let mut model = starter_model();
        model.nodes.extend([
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Split".into(),
                name: "Parallel".into(),
                kind: ProcessNodeKind::ParallelGateway,
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Terminate_A".into(),
                name: "Stop A".into(),
                kind: ProcessNodeKind::TerminateEnd,
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Terminate_B".into(),
                name: "Stop B".into(),
                kind: ProcessNodeKind::TerminateEnd,
            },
        ]);
        model.sequence_flows[0].target_id = "Split".into();
        model.sequence_flows.extend([
            ProcessSequenceFlow {
                call_start_node_id: None,
                id: "To_A".into(),
                source_id: "Split".into(),
                target_id: "Terminate_A".into(),
                condition: None,
            },
            ProcessSequenceFlow {
                call_start_node_id: None,
                id: "To_B".into(),
                source_id: "Split".into(),
                target_id: "Terminate_B".into(),
                condition: None,
            },
        ]);
        model.nodes.retain(|node| node.id != "End_1");
        model
            .diagram
            .shapes
            .retain(|shape| shape.element_id != "End_1");
        validate_model(&model).unwrap();
        let pair = &gateway_pairs(&model.nodes, &model.sequence_flows).unwrap()["Split"];
        assert_eq!(pair.join_node_id, None);
        assert_eq!(
            pair.branches["To_A"].terminate_end_node_ids,
            BTreeSet::from(["Terminate_A".into()])
        );
        assert_eq!(
            pair.branches["To_B"].terminate_end_node_ids,
            BTreeSet::from(["Terminate_B".into()])
        );
        assert!(pair
            .branches
            .values()
            .all(|branch| branch.join_incoming_edge_id.is_none()));

        model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Terminate_B")
            .unwrap()
            .kind = ProcessNodeKind::End;
        assert!(
            validate_model(&model).is_err(),
            "ordinary End cannot close an active fork"
        );
    }

    #[test]
    fn mixed_terminal_and_join_branches_keep_exact_join_capability() {
        let mut model = starter_model();
        model.nodes.extend([
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Split".into(),
                name: "Select".into(),
                kind: ProcessNodeKind::InclusiveGateway {
                    default_flow_id: Some("To_Stop".into()),
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Join".into(),
                name: "Join".into(),
                kind: ProcessNodeKind::InclusiveGateway {
                    default_flow_id: None,
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Wait_A".into(),
                name: "A".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Wait_B".into(),
                name: "B".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Stop".into(),
                name: "Stop".into(),
                kind: ProcessNodeKind::TerminateEnd,
            },
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
            model.sequence_flows.push(ProcessSequenceFlow {
                call_start_node_id: None,
                id: id.into(),
                source_id: source.into(),
                target_id: target.into(),
                condition: condition.map(str::to_string),
            });
        }
        validate_model(&model).unwrap();
        let pair = &gateway_pairs(&model.nodes, &model.sequence_flows).unwrap()["Split"];
        assert_eq!(pair.join_node_id.as_deref(), Some("Join"));
        assert_eq!(
            pair.branches["To_A"].join_incoming_edge_id.as_deref(),
            Some("From_A")
        );
        assert_eq!(
            pair.branches["To_B"].join_incoming_edge_id.as_deref(),
            Some("From_B")
        );
        assert_eq!(
            pair.branches["To_Stop"].terminate_end_node_ids,
            BTreeSet::from(["Stop".into()])
        );
        assert_eq!(pair.branches["To_Stop"].join_incoming_edge_id, None);
    }

    #[test]
    fn nested_mixed_gateways_keep_lifo_joins_and_shared_terminal_frontiers() {
        let mut model = starter_model();
        model.nodes.extend([
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "OuterSplit".into(),
                name: "Outer OR".into(),
                kind: ProcessNodeKind::InclusiveGateway {
                    default_flow_id: Some("Outer_Stop".into()),
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "OuterLeft".into(),
                name: "Outer left".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "InnerSplit".into(),
                name: "Inner AND".into(),
                kind: ProcessNodeKind::ParallelGateway,
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "InnerLeft".into(),
                name: "Inner left".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "InnerRight".into(),
                name: "Inner right".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "InnerJoin".into(),
                name: "Inner join".into(),
                kind: ProcessNodeKind::ParallelGateway,
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "OuterJoin".into(),
                name: "Outer join".into(),
                kind: ProcessNodeKind::InclusiveGateway {
                    default_flow_id: None,
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "SharedStop".into(),
                name: "Shared terminal".into(),
                kind: ProcessNodeKind::TerminateEnd,
            },
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
                call_start_node_id: None,
                id: id.into(),
                source_id: source.into(),
                target_id: target.into(),
                condition: condition.map(str::to_string),
            });
        }
        validate_model(&model).unwrap();
        let pairs = gateway_pairs(&model.nodes, &model.sequence_flows).unwrap();
        let outer = &pairs["OuterSplit"];
        assert_eq!(outer.join_node_id.as_deref(), Some("OuterJoin"));
        assert_eq!(
            outer.branches["Outer_Left"]
                .join_incoming_edge_id
                .as_deref(),
            Some("OuterLeft_Join")
        );
        assert_eq!(
            outer.branches["Outer_Inner"]
                .join_incoming_edge_id
                .as_deref(),
            Some("Inner_Outer")
        );
        assert_eq!(
            outer.branches["Outer_Inner"].terminate_end_node_ids,
            BTreeSet::from(["SharedStop".into()])
        );
        assert_eq!(outer.branches["Outer_Stop"].join_incoming_edge_id, None);
        assert_eq!(
            outer.branches["Outer_Stop"].terminate_end_node_ids,
            BTreeSet::from(["SharedStop".into()])
        );
        let inner = &pairs["InnerSplit"];
        assert_eq!(inner.join_node_id.as_deref(), Some("InnerJoin"));
        assert_eq!(
            inner.branches["Inner_Stop"].terminate_end_node_ids,
            BTreeSet::from(["SharedStop".into()])
        );

        let mut shared_work = model.clone();
        shared_work.nodes.push(ProcessNode {
            activity_io: None,
            repeat: None,
            id: "SharedWork".into(),
            name: "Invalid shared work".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            },
        });
        for edge in &mut shared_work.sequence_flows {
            if edge.id == "Outer_Stop" || edge.id == "Inner_Stop" {
                edge.target_id = "SharedWork".into();
            }
        }
        shared_work.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Shared_Stop".into(),
            source_id: "SharedWork".into(),
            target_id: "SharedStop".into(),
            condition: None,
        });
        assert!(
            validate_model(&shared_work).is_err(),
            "branches may share the terminal, not prior work"
        );

        let mut one_input_join = model.clone();
        one_input_join
            .sequence_flows
            .iter_mut()
            .find(|edge| edge.id == "OuterLeft_Join")
            .unwrap()
            .target_id = "SharedStop".into();
        assert!(
            validate_model(&one_input_join).is_err(),
            "a one-input gateway cannot silently act as a join"
        );

        let mut orphan_inner_join = model;
        orphan_inner_join
            .sequence_flows
            .iter_mut()
            .find(|edge| edge.id == "InnerRight_Join")
            .unwrap()
            .target_id = "SharedStop".into();
        assert!(
            validate_model(&orphan_inner_join).is_err(),
            "the inner split needs its full paired join"
        );
    }

    #[test]
    fn event_race_accepts_disjoint_terminate_frontiers_after_one_shot_catches() {
        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model.nodes.extend([
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Race".into(),
                name: "First event".into(),
                kind: ProcessNodeKind::EventBasedGateway,
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Timer_A".into(),
                name: "First".into(),
                kind: ProcessNodeKind::TimerCatch {
                    timer: ProcessTimerSpec::Duration { seconds: 1 },
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Timer_B".into(),
                name: "Second".into(),
                kind: ProcessNodeKind::TimerCatch {
                    timer: ProcessTimerSpec::Duration { seconds: 2 },
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Stop_A".into(),
                name: "Stop first".into(),
                kind: ProcessNodeKind::TerminateEnd,
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Stop_B".into(),
                name: "Stop second".into(),
                kind: ProcessNodeKind::TerminateEnd,
            },
        ]);
        model.sequence_flows[0].target_id = "Race".into();
        for (id, source, target) in [
            ("To_A", "Race", "Timer_A"),
            ("To_B", "Race", "Timer_B"),
            ("From_A", "Timer_A", "Stop_A"),
            ("From_B", "Timer_B", "Stop_B"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                call_start_node_id: None,
                id: id.into(),
                source_id: source.into(),
                target_id: target.into(),
                condition: None,
            });
        }
        model.nodes.retain(|node| node.id != "End_1");
        model
            .diagram
            .shapes
            .retain(|shape| shape.element_id != "End_1");
        validate_model(&model).unwrap();
    }

    #[test]
    fn event_race_accepts_receive_and_signal_but_refuses_message_mixing_attached_boundaries_and_extra_incoming(
    ) {
        let mut model = starter_model();
        model.target_namespace = Some("urn:orders".into());
        model.timer_timezone = Some("UTC".into());
        model
            .messages
            .push(tentaflow_protocol::processes::ProcessMessageDeclaration {
                message_id: "Message_1".into(),
                name: "order.received".into(),
            });
        model
            .signals
            .push(tentaflow_protocol::processes::ProcessSignalDeclaration {
                signal_id: "Signal_1".into(),
                namespace_uri: "urn:orders".into(),
                name: "Order changed".into(),
            });
        model
            .variables
            .insert("case_key".into(), serde_json::json!("case-1"));
        model
            .variables
            .insert("received".into(), serde_json::Value::Null);
        model.nodes.extend([
            ProcessNode {
                activity_io: None,
                id: "Race_1".into(),
                name: "First event".into(),
                repeat: None,
                kind: ProcessNodeKind::EventBasedGateway,
            },
            ProcessNode {
                activity_io: None,
                id: "Receive_1".into(),
                name: "Receive".into(),
                repeat: None,
                kind: ProcessNodeKind::ReceiveTask {
                    message_ref: "Message_1".into(),
                    correlation_expression: "vars.case_key".into(),
                    output_mapping: BTreeMap::from([("received".into(), "outputs".into())]),
                },
            },
            ProcessNode {
                activity_io: None,
                id: "Signal_1_Catch".into(),
                name: "Signal".into(),
                repeat: None,
                kind: ProcessNodeKind::SignalCatch {
                    signal_ref: "Signal_1".into(),
                    output_mapping: BTreeMap::from([("received".into(), "outputs".into())]),
                },
            },
            ProcessNode {
                activity_io: None,
                id: "Timer_1".into(),
                name: "Timeout".into(),
                repeat: None,
                kind: ProcessNodeKind::TimerCatch {
                    timer: ProcessTimerSpec::Duration { seconds: 60 },
                },
            },
        ]);
        model.sequence_flows[0].target_id = "Race_1".into();
        for (id, source, target) in [
            ("To_Receive", "Race_1", "Receive_1"),
            ("To_Signal", "Race_1", "Signal_1_Catch"),
            ("To_Timer", "Race_1", "Timer_1"),
            ("From_Receive", "Receive_1", "End_1"),
            ("From_Signal", "Signal_1_Catch", "End_1"),
            ("From_Timer", "Timer_1", "End_1"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                call_start_node_id: None,
                id: id.into(),
                source_id: source.into(),
                target_id: target.into(),
                condition: None,
            });
        }
        validate_model(&model).unwrap();
        let mut mixed = model.clone();
        mixed
            .nodes
            .iter_mut()
            .find(|node| node.id == "Timer_1")
            .unwrap()
            .kind = ProcessNodeKind::MessageCatch {
            message_ref: "Message_1".into(),
            correlation_expression: "vars.case_key".into(),
            output_mapping: BTreeMap::new(),
        };
        mixed.timer_timezone = None;
        for candidate in [mixed.clone(), {
            mixed.sequence_flows.reverse();
            mixed
        }] {
            let error = validate_model(&candidate).unwrap_err();
            let profile = error.downcast_ref::<EventGatewayProfileError>().unwrap();
            assert_eq!(profile.node_id, "Race_1");
            assert!(profile
                .reason
                .contains("cannot mix ReceiveTask with MessageCatch"));
        }
        let mut attached = model.clone();
        attached.nodes.push(ProcessNode {
            activity_io: None,
            id: "Receive_Boundary".into(),
            name: "Deadline".into(),
            repeat: None,
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Receive_1".into(),
                cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 30 },
            },
        });
        attached.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Boundary_End".into(),
            source_id: "Receive_Boundary".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        let error = validate_model(&attached).unwrap_err();
        let profile = error.downcast_ref::<EventGatewayProfileError>().unwrap();
        assert_eq!(profile.node_id, "Receive_Boundary");
        assert!(profile.reason.contains("cannot have attached boundary"));
        let mut extra = model.clone();
        extra.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Extra_Receive".into(),
            source_id: "Signal_1_Catch".into(),
            target_id: "Receive_1".into(),
            condition: None,
        });
        let error = validate_model(&extra).unwrap_err();
        let profile = error.downcast_ref::<EventGatewayProfileError>().unwrap();
        assert_eq!(profile.node_id, "Receive_1");
        assert!(profile
            .reason
            .contains("must have one incoming and outgoing flow"));

        let mut child_nodes = model.nodes.clone();
        for node in &mut child_nodes {
            if node.id == "Start_1" {
                node.id = "LocalStart".into();
            }
            if node.id == "End_1" {
                node.id = "LocalEnd".into();
            }
        }
        let mut child_flows = model.sequence_flows.clone();
        for flow in &mut child_flows {
            if flow.id == "Flow_1" {
                flow.id = "LocalFlow_1".into();
            }
            if flow.source_id == "Start_1" {
                flow.source_id = "LocalStart".into();
            }
            if flow.target_id == "End_1" {
                flow.target_id = "LocalEnd".into();
            }
        }
        let mut embedded = starter_model();
        embedded.target_namespace = model.target_namespace.clone();
        embedded.timer_timezone = model.timer_timezone.clone();
        embedded.messages = model.messages.clone();
        embedded.signals = model.signals.clone();
        embedded.nodes.insert(
            1,
            ProcessNode {
                activity_io: None,
                id: "Scope_1".into(),
                name: "Event scope".into(),
                repeat: None,
                kind: ProcessNodeKind::SubProcess {
                    body: tentaflow_protocol::processes::ProcessSubProcess {
                        modeling: None,
                        nodes: child_nodes,
                        sequence_flows: child_flows,
                        variables: model.variables.clone(),
                        diagram: ProcessDiagram::default(),
                    },
                    input_mapping: BTreeMap::new(),
                    output_mapping: BTreeMap::new(),
                },
            },
        );
        embedded.sequence_flows[0].target_id = "Scope_1".into();
        embedded.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Scope_End".into(),
            source_id: "Scope_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&embedded).unwrap();
    }

    #[test]
    fn call_activity_and_error_end_require_exact_binding_and_closed_parallel_path() {
        use tentaflow_protocol::processes::{
            ProcessCallableReference, ProcessErrorDeclaration, ProcessNode, ProcessSequenceFlow,
        };

        let mut model = starter_model();
        model.errors.push(ProcessErrorDeclaration {
            error_id: "Error_Business".into(),
            name: "Rejected".into(),
            error_code: "BUSINESS.REJECTED".into(),
        });
        model.nodes.insert(
            1,
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Call_1".into(),
                name: "Review".into(),
                kind: ProcessNodeKind::CallActivity(
                    tentaflow_protocol::processes::ProcessCallActivity {
                        target: ProcessCallTarget::PublishedBody {
                            definition_id: "91764f75-dadb-41aa-a252-a8a911fe7a94".into(),
                            version: 2,
                            called_element: ProcessCallableReference {
                                namespace_uri: "urn:example:review".into(),
                                process_id: "Review_Process".into(),
                            },
                        },
                        input_mapping: Default::default(),
                        output_mapping: Default::default(),
                    },
                ),
            },
        );
        model.nodes[2].kind = ProcessNodeKind::ErrorEnd {
            error_ref: "Error_Business".into(),
        };
        model.sequence_flows[0].target_id = "Call_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_2".into(),
            source_id: "Call_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&model).unwrap();

        if let ProcessNodeKind::CallActivity(call) = &mut model.nodes[1].kind {
            if let ProcessCallTarget::PublishedBody { version, .. } = &mut call.target {
                *version = 0;
            }
        }
        assert!(
            validate_model(&model).is_err(),
            "a call cannot choose latest at execution time"
        );
        if let ProcessNodeKind::CallActivity(call) = &mut model.nodes[1].kind {
            if let ProcessCallTarget::PublishedBody { version, .. } = &mut call.target {
                *version = 2;
            }
        }
        model.nodes[2].kind = ProcessNodeKind::ErrorEnd {
            error_ref: "Missing".into(),
        };
        assert!(
            validate_model(&model).is_err(),
            "an error end requires a declared business error"
        );
        model.nodes[2].kind = ProcessNodeKind::ErrorEnd {
            error_ref: "Error_Business".into(),
        };

        model.nodes.extend([
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Split_1".into(),
                name: "Split".into(),
                kind: ProcessNodeKind::ParallelGateway,
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Join_1".into(),
                name: "Join".into(),
                kind: ProcessNodeKind::ParallelGateway,
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Branch_A".into(),
                name: "A".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: Default::default(),
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Branch_B".into(),
                name: "B".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: Default::default(),
                },
            },
        ]);
        model.sequence_flows[0].target_id = "Split_1".into();
        for (id, source_id, target_id) in [
            ("Flow_3", "Split_1", "Branch_A"),
            ("Flow_4", "Split_1", "Branch_B"),
            ("Flow_5", "Branch_A", "Join_1"),
            ("Flow_6", "Branch_B", "Join_1"),
            ("Flow_7", "Join_1", "Call_1"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                call_start_node_id: None,
                id: id.into(),
                source_id: source_id.into(),
                target_id: target_id.into(),
                condition: None,
            });
        }
        validate_model(&model).unwrap();
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "Flow_5")
            .unwrap()
            .target_id = "End_1".into();
        assert!(
            validate_model(&model).is_err(),
            "an error end cannot silently consume an open parallel activation"
        );
    }

    #[test]
    fn message_declarations_keep_incomplete_drafts_but_publication_requires_real_references() {
        use tentaflow_protocol::processes::{ProcessErrorDeclaration, ProcessMessageDeclaration};
        let mut model = starter_model();
        model.target_namespace = Some("urn:example:customer:v1".into());
        model.messages.push(ProcessMessageDeclaration {
            message_id: "Message_Order".into(),
            name: "order.received".into(),
        });
        validate_draft(&model).unwrap();
        assert!(
            validate_model(&model).is_err(),
            "unused executable declaration cannot publish"
        );
        model.nodes[0].kind = ProcessNodeKind::MessageStart {
            message_ref: "Message_Order".into(),
            output_mapping: Default::default(),
        };
        validate_model(&model).unwrap();
        model.nodes[0].kind = ProcessNodeKind::MessageStart {
            message_ref: "Missing".into(),
            output_mapping: Default::default(),
        };
        validate_draft(&model).unwrap();
        assert!(
            validate_model(&model).is_err(),
            "a draft may retain a deleted reference but publication cannot"
        );
        model.nodes[0].kind = ProcessNodeKind::MessageStart {
            message_ref: "Message_Order".into(),
            output_mapping: Default::default(),
        };
        model.errors.push(ProcessErrorDeclaration {
            error_id: "Message_Order".into(),
            name: "bad".into(),
            error_code: "BUSINESS".into(),
        });
        assert!(
            validate_draft(&model).is_err(),
            "XML IDs are unique across declarations and nodes"
        );
        model.errors[0].error_id = "Error_Business".into();
        assert!(
            validate_model(&model).is_err(),
            "an unused error declaration cannot publish"
        );
    }

    #[test]
    fn event_gateway_accepts_disjoint_one_shot_branches_and_rejects_shared_activity() {
        use tentaflow_protocol::processes::{
            ProcessMessageDeclaration, ProcessNode, ProcessSequenceFlow,
        };
        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model.messages.push(ProcessMessageDeclaration {
            message_id: "Message_1".into(),
            name: "signal".into(),
        });
        model.nodes.extend([
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Race_1".into(),
                name: "First event".into(),
                kind: ProcessNodeKind::EventBasedGateway,
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Catch_Message".into(),
                name: "Message".into(),
                kind: ProcessNodeKind::MessageCatch {
                    message_ref: "Message_1".into(),
                    correlation_expression: "vars.case_id".into(),
                    output_mapping: Default::default(),
                },
            },
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Catch_Timer".into(),
                name: "Timeout".into(),
                kind: ProcessNodeKind::TimerCatch {
                    timer: ProcessTimerSpec::Duration { seconds: 60 },
                },
            },
        ]);
        model.sequence_flows[0].target_id = "Race_1".into();
        for (id, source, target) in [
            ("Flow_2", "Race_1", "Catch_Message"),
            ("Flow_3", "Race_1", "Catch_Timer"),
            ("Flow_4", "Catch_Message", "End_1"),
            ("Flow_5", "Catch_Timer", "End_1"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                call_start_node_id: None,
                id: id.into(),
                source_id: source.into(),
                target_id: target.into(),
                condition: None,
            });
        }
        validate_model(&model).unwrap();
        model.sequence_flows[4].target_id = "Catch_Message".into();
        assert!(
            validate_model(&model).is_err(),
            "a race child cannot be shared by both alternatives"
        );
    }

    #[test]
    fn calendar_without_timer_requires_zone_and_working_rule_requires_calendar() {
        use tentaflow_protocol::processes::{HolidayPolicy, ProcessWorkCalendar, WorkWindow};
        let mut model = starter_model();
        model.work_calendar = Some(ProcessWorkCalendar {
            name: "Office".into(),
            weekly_windows: vec![WorkWindow {
                weekday: 1,
                start_minute: 540,
                end_minute: 1020,
            }],
            manual_days_off: Vec::new(),
            holiday_policy: HolidayPolicy::None,
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
            name: "Office".into(),
            weekly_windows: Vec::new(),
            manual_days_off: Vec::new(),
            holiday_policy: HolidayPolicy::None,
        });
        assert!(validate_model(&model).is_err());
    }

    fn boundary_model() -> ProcessModel {
        use tentaflow_protocol::processes::{ProcessNode, ProcessSequenceFlow};

        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model.nodes.push(ProcessNode {
            activity_io: None,
            repeat: None,
            id: "Review_1".into(),
            name: "Review".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: Default::default(),
            },
        });
        model.nodes.push(ProcessNode {
            activity_io: None,
            repeat: None,
            id: "Boundary_A".into(),
            name: "Time limit".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Review_1".into(),
                cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 90 },
            },
        });
        model.nodes.push(ProcessNode {
            activity_io: None,
            repeat: None,
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
                call_start_node_id: None,
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
        let boundary = model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Boundary_A")
            .unwrap();
        if let ProcessNodeKind::BoundaryTimer { attached_to_id, .. } = &mut boundary.kind {
            *attached_to_id = "End_1".into();
        }
        assert!(validate_model(&model).is_err());
        model = boundary_model();
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_5".into(),
            source_id: "Review_1".into(),
            target_id: "Boundary_A".into(),
            condition: None,
        });
        assert!(validate_model(&model).is_err());
        model = boundary_model();
        model.nodes.push(ProcessNode {
            activity_io: None,
            repeat: None,
            id: "Shared_1".into(),
            name: "Shared".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: Default::default(),
            },
        });
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "Flow_2")
            .unwrap()
            .target_id = "Shared_1".into();
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "Flow_3")
            .unwrap()
            .target_id = "Shared_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_5".into(),
            source_id: "Shared_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        assert!(validate_model(&model).is_err());
    }

    #[test]
    fn boundary_after_paired_join_and_own_closed_parallel_region_are_supported() {
        use tentaflow_protocol::processes::{ProcessNode, ProcessSequenceFlow};

        let mut model = boundary_model();
        for id in ["Split_1", "Join_1", "SideSplit_1", "SideJoin_1"] {
            model.nodes.push(ProcessNode {
                activity_io: None,
                repeat: None,
                id: id.into(),
                name: id.into(),
                kind: ProcessNodeKind::ParallelGateway,
            });
        }
        for id in ["Branch_A", "Branch_B", "Side_A", "Side_B"] {
            model.nodes.push(ProcessNode {
                activity_io: None,
                repeat: None,
                id: id.into(),
                name: id.into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: Default::default(),
                },
            });
        }
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "Flow_1")
            .unwrap()
            .target_id = "Split_1".into();
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "Flow_3")
            .unwrap()
            .target_id = "SideSplit_1".into();
        for (id, source, target) in [
            ("F_A", "Split_1", "Branch_A"),
            ("F_B", "Split_1", "Branch_B"),
            ("F_C", "Branch_A", "Join_1"),
            ("F_D", "Branch_B", "Join_1"),
            ("F_E", "Join_1", "Review_1"),
            ("F_F", "SideSplit_1", "Side_A"),
            ("F_G", "SideSplit_1", "Side_B"),
            ("F_H", "Side_A", "SideJoin_1"),
            ("F_I", "Side_B", "SideJoin_1"),
            ("F_J", "SideJoin_1", "End_1"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                call_start_node_id: None,
                id: id.into(),
                source_id: source.into(),
                target_id: target.into(),
                condition: None,
            });
        }
        validate_model(&model).unwrap();
        let boundary = model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Boundary_A")
            .unwrap();
        if let ProcessNodeKind::BoundaryTimer { attached_to_id, .. } = &mut boundary.kind {
            *attached_to_id = "Branch_A".into();
        }
        assert!(validate_model(&model).is_err());
    }

    #[test]
    fn timer_profiles_require_explicit_zone_and_supported_start_catch_rules() {
        let mut model = starter_model();
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::Cycle {
                seconds: 300,
                total_firings: Some(3),
            },
        };
        assert!(validate_model(&model).is_err());
        model.timer_timezone = Some("Europe/Warsaw".into());
        validate_model(&model).unwrap();
        model.timer_timezone = Some("Mars/Olympus".into());
        assert!(validate_model(&model).is_err());
        model.timer_timezone = Some("UTC".into());
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::Cycle {
                seconds: 299,
                total_firings: Some(3),
            },
        };
        assert!(validate_model(&model).is_err());
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::Daily {
                hour: 24,
                minute: 0,
                total_firings: None,
            },
        };
        assert!(validate_model(&model).is_err());
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::Date {
                at: "2027-01-02T03:04:05.1234Z".into(),
            },
        };
        assert!(validate_model(&model).is_err());
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::Date {
                at: "2027-01-02T03:04:05.123Z".into(),
            },
        };
        validate_model(&model).unwrap();
        model.nodes.insert(
            1,
            tentaflow_protocol::processes::ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Wait_1".into(),
                name: "Wait".into(),
                kind: ProcessNodeKind::TimerCatch {
                    timer: ProcessTimerSpec::Cycle {
                        seconds: 300,
                        total_firings: None,
                    },
                },
            },
        );
        model.sequence_flows[0].target_id = "Wait_1".into();
        model
            .sequence_flows
            .push(tentaflow_protocol::processes::ProcessSequenceFlow {
                call_start_node_id: None,
                id: "Flow_2".into(),
                source_id: "Wait_1".into(),
                target_id: "End_1".into(),
                condition: None,
            });
        assert!(validate_model(&model).is_err());
        model.nodes[1].kind = ProcessNodeKind::TimerCatch {
            timer: ProcessTimerSpec::Duration { seconds: 90 },
        };
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
                call_start_node_id: None,
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
        model.nodes.insert(
            1,
            ProcessNode {
                activity_io: None,
                repeat: None,
                id: "Sub_1".into(),
                name: "Review scope".into(),
                kind: ProcessNodeKind::SubProcess {
                    body: ProcessSubProcess {
                        modeling: None,
                        nodes: vec![
                            ProcessNode {
                                activity_io: None,
                                repeat: None,
                                id: "LocalStart".into(),
                                name: "Start".into(),
                                kind: ProcessNodeKind::Start,
                            },
                            ProcessNode {
                                activity_io: None,
                                repeat: None,
                                id: "LocalTask".into(),
                                name: "Approve".into(),
                                kind: ProcessNodeKind::UserTask {
                                    assignee_user_id: None,
                                    output_mapping: BTreeMap::new(),
                                },
                            },
                            ProcessNode {
                                activity_io: None,
                                repeat: None,
                                id: "LocalEnd".into(),
                                name: "End".into(),
                                kind: ProcessNodeKind::End,
                            },
                        ],
                        sequence_flows: vec![
                            ProcessSequenceFlow {
                                call_start_node_id: None,
                                id: "LocalFlow1".into(),
                                source_id: "LocalStart".into(),
                                target_id: "LocalTask".into(),
                                condition: None,
                            },
                            ProcessSequenceFlow {
                                call_start_node_id: None,
                                id: "LocalFlow2".into(),
                                source_id: "LocalTask".into(),
                                target_id: "LocalEnd".into(),
                                condition: None,
                            },
                        ],
                        variables: BTreeMap::from([(
                            "local_ID".into(),
                            serde_json::json!("value"),
                        )]),
                        diagram: ProcessDiagram::default(),
                    },
                    input_mapping: BTreeMap::new(),
                    output_mapping: BTreeMap::new(),
                },
            },
        );
        model.sequence_flows[0].target_id = "Sub_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_2".into(),
            source_id: "Sub_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        model
    }

    #[test]
    fn embedded_terminate_end_is_local_to_its_own_body() {
        let mut model = embedded_model();
        if let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes[1].kind {
            body.nodes[2].kind = ProcessNodeKind::TerminateEnd;
        }
        model
            .variables
            .insert("results".into(), serde_json::json!([]));
        model.timer_timezone = Some("UTC".into());
        model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
        model.nodes.push(ProcessNode {
            activity_io: None,
            id: "OuterDeadline".into(),
            name: "Group deadline".into(),
            repeat: None,
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Sub_1".into(),
                cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 90 },
            },
        });
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "OuterDeadlineExit".into(),
            source_id: "OuterDeadline".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&model).unwrap();
        assert!(matches!(
            scope_body(&model, &["Sub_1".into()]).unwrap().0[2].kind,
            ProcessNodeKind::TerminateEnd
        ));
        assert!(matches!(model.nodes[2].kind, ProcessNodeKind::End));
        assert!(matches!(
            model.nodes[1].repeat,
            Some(ProcessRepeatSpec::MultiInstance { .. })
        ));
        assert!(matches!(
            model.nodes.last().unwrap().kind,
            ProcessNodeKind::BoundaryTimer { .. }
        ));
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
            body.nodes[0].kind = ProcessNodeKind::TimerStart {
                timer: ProcessTimerSpec::Duration { seconds: 60 },
            };
        }
        model.timer_timezone = Some("UTC".into());
        assert!(validate_model(&model).is_err());
    }

    #[test]
    fn repeated_tasks_require_bounded_input_and_a_declared_local_output() {
        use tentaflow_protocol::processes::{ActivityVerification, ProcessMultiInstanceMode};
        let mut model = starter_model();
        model
            .variables
            .insert("results".into(), serde_json::json!([]));
        model
            .variables
            .insert("items".into(), serde_json::json!([{"id": 1}]));
        model.nodes.insert(
            1,
            ProcessNode {
                activity_io: None,
                id: "Review_1".into(),
                name: "Review".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
                repeat: Some(ProcessRepeatSpec::MultiInstance {
                    mode: ProcessMultiInstanceMode::Parallel,
                    input: ProcessMultiInstanceInput::Cardinality { count: 16 },
                    output_collection_variable: "results".into(),
                }),
            },
        );
        model.sequence_flows[0].target_id = "Review_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_2".into(),
            source_id: "Review_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&model).unwrap();
        model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 0 },
            output_collection_variable: "results".into(),
        });
        validate_model(&model).unwrap();
        model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::CollectionExpression {
                expression: "vars.items".into(),
            },
            output_collection_variable: "results".into(),
        });
        validate_model(&model).unwrap();
        model.nodes[1].repeat = Some(ProcessRepeatSpec::StructuredLoop {
            condition: "vars.keep_going".into(),
            test_before: true,
            max_iterations: 32,
            output_collection_variable: "results".into(),
        });
        validate_model(&model).unwrap();
        let old = model.nodes[1].repeat.clone();
        for invalid in [
            ProcessRepeatSpec::MultiInstance {
                mode: ProcessMultiInstanceMode::Parallel,
                input: ProcessMultiInstanceInput::Cardinality { count: 17 },
                output_collection_variable: "results".into(),
            },
            ProcessRepeatSpec::StructuredLoop {
                condition: "vars.keep_going".into(),
                test_before: true,
                max_iterations: 0,
                output_collection_variable: "results".into(),
            },
            ProcessRepeatSpec::StructuredLoop {
                condition: "vars.keep_going".into(),
                test_before: true,
                max_iterations: 33,
                output_collection_variable: "results".into(),
            },
            ProcessRepeatSpec::StructuredLoop {
                condition: "vars.keep_going".into(),
                test_before: true,
                max_iterations: 2,
                output_collection_variable: "missing".into(),
            },
        ] {
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
            assignee_user_id: None,
            output_mapping: BTreeMap::new(),
        };
        model.timer_timezone = Some("UTC".into());
        model.nodes.push(ProcessNode {
            activity_io: None,
            id: "ReviewDeadline".into(),
            name: "Deadline".into(),
            repeat: None,
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Review_1".into(),
                cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 60 },
            },
        });
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "DeadlineExit".into(),
            source_id: "ReviewDeadline".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&model).unwrap();

        let mut service = starter_model();
        service
            .variables
            .insert("results".into(), serde_json::json!([]));
        service.nodes.insert(
            1,
            ProcessNode {
                activity_io: None,
                id: "ServiceReview".into(),
                name: "Review".into(),
                kind: ProcessNodeKind::ServiceTask {
                    flow_id: "flow-review".into(),
                    input_mapping: BTreeMap::new(),
                    output_mapping: BTreeMap::new(),
                    verification: ActivityVerification::Human,
                    timeout_seconds: 60,
                    result_expression: None,
                },
                repeat: Some(ProcessRepeatSpec::MultiInstance {
                    mode: ProcessMultiInstanceMode::Sequential,
                    input: ProcessMultiInstanceInput::Cardinality { count: 1 },
                    output_collection_variable: "results".into(),
                }),
            },
        );
        service.sequence_flows[0].target_id = "ServiceReview".into();
        service.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "ServiceExit".into(),
            source_id: "ServiceReview".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&service).unwrap();
        service
            .messages
            .push(tentaflow_protocol::processes::ProcessMessageDeclaration {
                message_id: "Message_1".into(),
                name: "service.competitor".into(),
            });
        service
            .variables
            .insert("case_key".into(), serde_json::json!("case-1"));
        service.nodes.push(ProcessNode {
            activity_io: None,
            id: "ServiceMessage".into(),
            name: "Reply".into(),
            repeat: None,
            kind: ProcessNodeKind::BoundaryMessage {
                attached_to_id: "ServiceReview".into(),
                cancel_activity: false,
                message_ref: "Message_1".into(),
                correlation_expression: "vars.case_key".into(),
                output_mapping: BTreeMap::new(),
            },
        });
        service.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "ServiceMessageExit".into(),
            source_id: "ServiceMessage".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&service).unwrap();

        let mut embedded = embedded_model();
        embedded
            .variables
            .insert("root_only".into(), serde_json::json!([]));
        if let ProcessNodeKind::SubProcess { body, .. } = &mut embedded.nodes[1].kind {
            body.variables
                .insert("local_results".into(), serde_json::json!([]));
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
        model
            .variables
            .insert("amount".into(), serde_json::json!(2));
        model
            .variables
            .insert("answer".into(), serde_json::Value::Null);
        model
            .variables
            .insert("results".into(), serde_json::json!([]));
        model.nodes.insert(
            1,
            ProcessNode {
                activity_io: None,
                id: "Script_1".into(),
                name: "Calculate".into(),
                repeat: None,
                kind: ProcessNodeKind::ScriptTask {
                    script: String::new(),
                    output_mapping: BTreeMap::new(),
                },
            },
        );
        model.sequence_flows[0].target_id = "Script_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_2".into(),
            source_id: "Script_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_draft(&model).unwrap();
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("requires an expression"));
        let ProcessNodeKind::ScriptTask {
            script,
            output_mapping,
        } = &mut model.nodes[1].kind
        else {
            unreachable!()
        };
        *script = "vars.amount + 1".into();
        output_mapping.insert("answer".into(), "outputs".into());
        validate_model(&model).unwrap();
        let ProcessNodeKind::ScriptTask { script, .. } = &mut model.nodes[1].kind else {
            unreachable!()
        };
        *script = "[".into();
        assert!(
            validate_draft(&model).is_err(),
            "a provided invalid body cannot be saved"
        );
        let ProcessNodeKind::ScriptTask { script, .. } = &mut model.nodes[1].kind else {
            unreachable!()
        };
        *script = "vars.amount + 1".into();
        let ProcessNodeKind::ScriptTask { output_mapping, .. } = &mut model.nodes[1].kind else {
            unreachable!()
        };
        output_mapping.insert("answer".into(), "[".into());
        assert!(
            validate_draft(&model).is_err(),
            "a provided invalid mapping cannot be saved"
        );
        let ProcessNodeKind::ScriptTask { output_mapping, .. } = &mut model.nodes[1].kind else {
            unreachable!()
        };
        output_mapping.insert("answer".into(), "outputs".into());
        model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 1 },
            output_collection_variable: "results".into(),
        });
        validate_draft(&model).unwrap();
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("ScriptTask MI requires empty output mapping"));
        let ProcessNodeKind::ScriptTask { output_mapping, .. } = &mut model.nodes[1].kind else {
            unreachable!()
        };
        output_mapping.clear();
        validate_model(&model).unwrap();
        model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::CollectionExpression {
                expression: "vars.items".into(),
            },
            output_collection_variable: "results".into(),
        });
        model
            .variables
            .insert("items".into(), serde_json::json!([null, 4, {"case": "A"}]));
        validate_model(&model).unwrap();
        model.nodes[1].repeat = Some(ProcessRepeatSpec::StructuredLoop {
            condition: "vars.answer < 3".into(),
            test_before: false,
            max_iterations: 32,
            output_collection_variable: "results".into(),
        });
        if let ProcessNodeKind::ScriptTask { output_mapping, .. } = &mut model.nodes[1].kind {
            output_mapping.insert("answer".into(), "outputs".into());
        }
        validate_model(&model).unwrap();
        if let ProcessNodeKind::ScriptTask { output_mapping, .. } = &mut model.nodes[1].kind {
            output_mapping.insert("answer".into(), "[".into());
        }
        assert!(
            validate_draft(&model).is_err(),
            "a provided invalid loop mapping cannot be saved"
        );
    }

    #[test]
    fn repeated_script_accepts_one_outer_timer_or_message_boundary_without_error_producer() {
        let mut model = starter_model();
        model
            .variables
            .insert("results".into(), serde_json::json!([]));
        model
            .variables
            .insert("case_key".into(), serde_json::json!("case-a"));
        model.nodes.insert(
            1,
            ProcessNode {
                activity_io: None,
                id: "Script_1".into(),
                name: "Calculate <&>".into(),
                repeat: Some(ProcessRepeatSpec::MultiInstance {
                    mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Parallel,
                    input: ProcessMultiInstanceInput::Cardinality { count: 2 },
                    output_collection_variable: "results".into(),
                }),
                kind: ProcessNodeKind::ScriptTask {
                    script: "null".into(),
                    output_mapping: BTreeMap::new(),
                },
            },
        );
        model.sequence_flows[0].target_id = "Script_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "ScriptExit".into(),
            source_id: "Script_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        model.timer_timezone = Some("UTC".into());
        model.nodes.push(ProcessNode {
            activity_io: None,
            id: "ScriptTimer".into(),
            name: "Deadline".into(),
            repeat: None,
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Script_1".into(),
                cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 60 },
            },
        });
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "ScriptTimerExit".into(),
            source_id: "ScriptTimer".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&model).unwrap();
        if let ProcessNodeKind::BoundaryTimer {
            cancel_activity, ..
        } = &mut model.nodes.last_mut().unwrap().kind
        {
            *cancel_activity = false;
        }
        validate_model(&model).unwrap();
        model.nodes.pop();
        model.sequence_flows.pop();
        model.timer_timezone = None;
        model
            .messages
            .push(tentaflow_protocol::processes::ProcessMessageDeclaration {
                message_id: "ReplyMessage".into(),
                name: "reply.received".into(),
            });
        model.nodes.push(ProcessNode {
            activity_io: None,
            id: "ScriptMessage".into(),
            name: "Reply".into(),
            repeat: None,
            kind: ProcessNodeKind::BoundaryMessage {
                attached_to_id: "Script_1".into(),
                cancel_activity: false,
                message_ref: "ReplyMessage".into(),
                correlation_expression: "vars.case_key".into(),
                output_mapping: BTreeMap::new(),
            },
        });
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "ScriptMessageExit".into(),
            source_id: "ScriptMessage".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&model).unwrap();
        model.nodes.pop();
        model.sequence_flows.pop();
        model.messages.clear();
        model.nodes.push(ProcessNode {
            activity_io: None,
            id: "ScriptError".into(),
            name: "Error".into(),
            repeat: None,
            kind: ProcessNodeKind::BoundaryError {
                attached_to_id: "Script_1".into(),
                error_ref: None,
                output_mapping: BTreeMap::new(),
            },
        });
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "ScriptErrorExit".into(),
            source_id: "ScriptError".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
    }

    #[test]
    fn script_task_is_immediate_before_a_real_escalation_wait() {
        let mut model = escalation_model();
        model.nodes.push(ProcessNode {
            activity_io: None,
            id: "Script_1".into(),
            name: "Prepare".into(),
            repeat: None,
            kind: ProcessNodeKind::ScriptTask {
                script: "null".into(),
                output_mapping: BTreeMap::new(),
            },
        });
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "Flow_Escalation")
            .unwrap()
            .target_id = "Script_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_Script_Wait".into(),
            source_id: "Script_1".into(),
            target_id: "Wait_1".into(),
            condition: None,
        });
        validate_model(&model).unwrap();
    }

    #[test]
    fn manual_task_requires_publishable_external_instructions_and_bounded_repeat() {
        let mut model = starter_model();
        model.nodes.insert(
            1,
            ProcessNode {
                activity_io: None,
                id: "Manual_1".into(),
                name: "External work".into(),
                repeat: None,
                kind: ProcessNodeKind::ManualTask {
                    assignee_user_id: None,
                    instructions: String::new(),
                },
            },
        );
        model.sequence_flows[0].target_id = "Manual_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_2".into(),
            source_id: "Manual_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_draft(&model).unwrap();
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("requires instructions"));
        model.nodes[1].kind = ProcessNodeKind::ManualTask {
            assignee_user_id: Some("worker-2".into()),
            instructions: "Check the external register.\nAcknowledge here.".into(),
        };
        validate_model(&model).unwrap();
        model.nodes[1].kind = ProcessNodeKind::ManualTask {
            assignee_user_id: Some("worker-2".into()),
            instructions: "invalid\rline".into(),
        };
        assert!(validate_draft(&model)
            .unwrap_err()
            .to_string()
            .contains("invalid instructions"));
        model.nodes[1].kind = ProcessNodeKind::ManualTask {
            assignee_user_id: Some("worker-2".into()),
            instructions: "x".repeat(4097),
        };
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("invalid instructions"));
        model.nodes[1].kind = ProcessNodeKind::ManualTask {
            assignee_user_id: Some("worker-2".into()),
            instructions: "Check the external register".into(),
        };
        model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
        model
            .variables
            .insert("results".into(), serde_json::json!([]));
        validate_model(&model).unwrap();
    }

    #[test]
    fn seven_activities_accept_bounded_repeat_with_one_outer_timer_or_message_boundary() {
        use tentaflow_protocol::processes::{
            ActivityVerification, ProcessCallableReference, ProcessSubProcess,
        };
        let kinds = [
            ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            },
            ProcessNodeKind::ServiceTask {
                flow_id: "flow-review".into(),
                input_mapping: BTreeMap::new(),
                output_mapping: BTreeMap::new(),
                verification: ActivityVerification::Human,
                timeout_seconds: 60,
                result_expression: None,
            },
            ProcessNodeKind::ManualTask {
                assignee_user_id: None,
                instructions: "Check the external record before acknowledgment".into(),
            },
            ProcessNodeKind::SendTask {
                message_ref: "Message_1".into(),
                target: ProcessMessageTargetSpec::Start {
                    definition_id: uuid::Uuid::nil().to_string(),
                    process_id: None,
                    start_node_id: None,
                },
                correlation_expression: "vars.case_key".into(),
                payload_expression: "vars.payload".into(),
                ttl_seconds: 60,
            },
            ProcessNodeKind::ReceiveTask {
                message_ref: "Message_1".into(),
                correlation_expression: "vars.case_key".into(),
                output_mapping: BTreeMap::new(),
            },
            ProcessNodeKind::SubProcess {
                body: ProcessSubProcess {
                    modeling: None,
                    nodes: vec![
                        ProcessNode {
                            activity_io: None,
                            id: "ChildStart".into(),
                            name: "Start".into(),
                            repeat: None,
                            kind: ProcessNodeKind::Start,
                        },
                        ProcessNode {
                            activity_io: None,
                            id: "ChildEnd".into(),
                            name: "End".into(),
                            repeat: None,
                            kind: ProcessNodeKind::End,
                        },
                    ],
                    sequence_flows: vec![ProcessSequenceFlow {
                        call_start_node_id: None,
                        id: "ChildFlow".into(),
                        source_id: "ChildStart".into(),
                        target_id: "ChildEnd".into(),
                        condition: None,
                    }],
                    variables: BTreeMap::new(),
                    diagram: ProcessDiagram::default(),
                },
                input_mapping: BTreeMap::new(),
                output_mapping: BTreeMap::new(),
            },
            ProcessNodeKind::CallActivity(tentaflow_protocol::processes::ProcessCallActivity {
                target: ProcessCallTarget::PublishedBody {
                    definition_id: "91764f75-dadb-41aa-a252-a8a911fe7a94".into(),
                    version: 2,
                    called_element: ProcessCallableReference {
                        namespace_uri: "urn:review".into(),
                        process_id: "Review_Process".into(),
                    },
                },
                input_mapping: BTreeMap::new(),
                output_mapping: BTreeMap::new(),
            }),
        ];
        for kind in kinds {
            let mut model = starter_model();
            if matches!(
                &kind,
                ProcessNodeKind::SendTask { .. } | ProcessNodeKind::ReceiveTask { .. }
            ) {
                model
                    .messages
                    .push(tentaflow_protocol::processes::ProcessMessageDeclaration {
                        message_id: "Message_1".into(),
                        name: "order.received".into(),
                    });
            }
            model
                .variables
                .insert("case_key".into(), serde_json::json!("case-1"));
            model
                .variables
                .insert("payload".into(), serde_json::json!({"case":1}));
            model
                .variables
                .insert("results".into(), serde_json::json!([]));
            model
                .variables
                .insert("items".into(), serde_json::json!([null, {"case":1}]));
            model.nodes.insert(
                1,
                ProcessNode {
                    activity_io: None,
                    id: "Repeat_1".into(),
                    name: "Repeat".into(),
                    repeat: None,
                    kind,
                },
            );
            model.sequence_flows[0].target_id = "Repeat_1".into();
            model.sequence_flows.push(ProcessSequenceFlow {
                call_start_node_id: None,
                id: "RepeatExit".into(),
                source_id: "Repeat_1".into(),
                target_id: "End_1".into(),
                condition: None,
            });
            for repeat in [
                ProcessRepeatSpec::MultiInstance {
                    mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
                    input: ProcessMultiInstanceInput::CollectionExpression {
                        expression: "vars.items".into(),
                    },
                    output_collection_variable: "results".into(),
                },
                ProcessRepeatSpec::MultiInstance {
                    mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Parallel,
                    input: ProcessMultiInstanceInput::Cardinality { count: 16 },
                    output_collection_variable: "results".into(),
                },
                ProcessRepeatSpec::StructuredLoop {
                    condition: "vars.more".into(),
                    test_before: true,
                    max_iterations: 32,
                    output_collection_variable: "results".into(),
                },
            ] {
                model.nodes[1].repeat = Some(repeat);
                validate_model(&model).unwrap();
                model.timer_timezone = Some("UTC".into());
                model.nodes.push(ProcessNode {
                    activity_io: None,
                    id: "RepeatDeadline".into(),
                    name: "Deadline".into(),
                    repeat: None,
                    kind: ProcessNodeKind::BoundaryTimer {
                        attached_to_id: "Repeat_1".into(),
                        cancel_activity: true,
                        timer: ProcessTimerSpec::Duration { seconds: 60 },
                    },
                });
                model.sequence_flows.push(ProcessSequenceFlow {
                    call_start_node_id: None,
                    id: "DeadlineExit".into(),
                    source_id: "RepeatDeadline".into(),
                    target_id: "End_1".into(),
                    condition: None,
                });
                validate_model(&model).unwrap();
                if let ProcessNodeKind::BoundaryTimer {
                    cancel_activity, ..
                } = &mut model.nodes.last_mut().unwrap().kind
                {
                    *cancel_activity = false;
                }
                validate_model(&model).unwrap();
                model.nodes.pop();
                model.sequence_flows.pop();
                model.timer_timezone = None;
                if model.messages.is_empty() {
                    model
                        .messages
                        .push(tentaflow_protocol::processes::ProcessMessageDeclaration {
                            message_id: "Message_1".into(),
                            name: "order.received".into(),
                        });
                }
                model.nodes.push(ProcessNode {
                    activity_io: None,
                    id: "RepeatMessage".into(),
                    name: "Message".into(),
                    repeat: None,
                    kind: ProcessNodeKind::BoundaryMessage {
                        attached_to_id: "Repeat_1".into(),
                        cancel_activity: false,
                        message_ref: "Message_1".into(),
                        correlation_expression: "vars.case_key".into(),
                        output_mapping: BTreeMap::new(),
                    },
                });
                model.sequence_flows.push(ProcessSequenceFlow {
                    call_start_node_id: None,
                    id: "MessageExit".into(),
                    source_id: "RepeatMessage".into(),
                    target_id: "End_1".into(),
                    condition: None,
                });
                validate_model(&model).unwrap();
                model.nodes.pop();
                model.sequence_flows.pop();
                if !matches!(
                    &model.nodes[1].kind,
                    ProcessNodeKind::SendTask { .. } | ProcessNodeKind::ReceiveTask { .. }
                ) {
                    model.messages.clear();
                }
            }
        }
    }

    #[test]
    fn repeated_service_retains_actual_error_and_escalation_boundary_routes() {
        let mut model = escalation_model();
        model
            .variables
            .insert("results".into(), serde_json::json!([]));
        model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Service_1")
            .unwrap()
            .repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
        validate_model(&model).unwrap();
        model.nodes.push(ProcessNode {
            activity_io: None,
            id: "Error_Boundary".into(),
            name: "Failed check".into(),
            repeat: None,
            kind: ProcessNodeKind::BoundaryError {
                attached_to_id: "Service_1".into(),
                error_ref: None,
                output_mapping: BTreeMap::new(),
            },
        });
        model.nodes.push(ProcessNode {
            activity_io: None,
            id: "Error_Wait".into(),
            name: "Review failure".into(),
            repeat: None,
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            },
        });
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_Error".into(),
            source_id: "Error_Boundary".into(),
            target_id: "Error_Wait".into(),
            condition: None,
        });
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_Error_End".into(),
            source_id: "Error_Wait".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&model).unwrap();
    }

    #[test]
    fn repeated_manual_wait_in_embedded_and_independently_called_models_keeps_local_output_scope() {
        use tentaflow_protocol::processes::{ProcessCallableReference, ProcessMultiInstanceMode};
        let mut embedded = embedded_model();
        if let ProcessNodeKind::SubProcess { body, .. } = &mut embedded.nodes[1].kind {
            body.variables
                .insert("local_results".into(), serde_json::json!([]));
            body.nodes[1].kind = ProcessNodeKind::ManualTask {
                assignee_user_id: None,
                instructions: "Check the external register".into(),
            };
            body.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
                mode: ProcessMultiInstanceMode::Parallel,
                input: ProcessMultiInstanceInput::Cardinality { count: 2 },
                output_collection_variable: "local_results".into(),
            });
        }
        validate_model(&embedded).unwrap();
        assert!(!embedded.variables.contains_key("local_results"));

        let mut called = starter_model();
        called
            .variables
            .insert("child_results".into(), serde_json::json!([]));
        called.nodes.insert(
            1,
            ProcessNode {
                activity_io: None,
                id: "ChildManual".into(),
                name: "External review".into(),
                kind: ProcessNodeKind::ManualTask {
                    assignee_user_id: None,
                    instructions: "Inspect the original document".into(),
                },
                repeat: Some(ProcessRepeatSpec::StructuredLoop {
                    condition: "vars.more".into(),
                    test_before: true,
                    max_iterations: 3,
                    output_collection_variable: "child_results".into(),
                }),
            },
        );
        called.sequence_flows[0].target_id = "ChildManual".into();
        called.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "ChildManualExit".into(),
            source_id: "ChildManual".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&called).unwrap();
        let mut parent = starter_model();
        parent.nodes.insert(
            1,
            ProcessNode {
                activity_io: None,
                id: "CallChild".into(),
                name: "Call pinned review".into(),
                repeat: None,
                kind: ProcessNodeKind::CallActivity(
                    tentaflow_protocol::processes::ProcessCallActivity {
                        target: ProcessCallTarget::PublishedBody {
                            definition_id: "91764f75-dadb-41aa-a252-a8a911fe7a94".into(),
                            version: 2,
                            called_element: ProcessCallableReference {
                                namespace_uri: "urn:review".into(),
                                process_id: called.process_id.clone(),
                            },
                        },
                        input_mapping: BTreeMap::new(),
                        output_mapping: BTreeMap::new(),
                    },
                ),
            },
        );
        parent.sequence_flows[0].target_id = "CallChild".into();
        parent.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "CallChildExit".into(),
            source_id: "CallChild".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&parent).unwrap();
        assert!(!parent.variables.contains_key("child_results"));
    }

    #[test]
    fn send_and_receive_tasks_require_declared_messages_and_accept_bounded_repeat() {
        let mut model = starter_model();
        model
            .messages
            .push(tentaflow_protocol::processes::ProcessMessageDeclaration {
                message_id: "Message_1".into(),
                name: "order.received".into(),
            });
        model.nodes.insert(
            1,
            ProcessNode {
                activity_io: None,
                id: "Send_1".into(),
                name: "Admit locally".into(),
                repeat: None,
                kind: ProcessNodeKind::SendTask {
                    message_ref: "Message_1".into(),
                    target: ProcessMessageTargetSpec::Start {
                        definition_id: uuid::Uuid::nil().to_string(),
                        process_id: None,
                        start_node_id: None,
                    },
                    correlation_expression: "vars.case_key".into(),
                    payload_expression: "vars.payload".into(),
                    ttl_seconds: 60,
                },
            },
        );
        model.nodes.insert(
            2,
            ProcessNode {
                activity_io: None,
                id: "Receive_1".into(),
                name: "Wait".into(),
                repeat: None,
                kind: ProcessNodeKind::ReceiveTask {
                    message_ref: "Message_1".into(),
                    correlation_expression: "vars.case_key".into(),
                    output_mapping: BTreeMap::new(),
                },
            },
        );
        model.sequence_flows[0].target_id = "Send_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_Send".into(),
            source_id: "Send_1".into(),
            target_id: "Receive_1".into(),
            condition: None,
        });
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_Receive".into(),
            source_id: "Receive_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        model.variables.insert(
            "case_key".into(),
            serde_json::Value::String("case-1".into()),
        );
        model
            .variables
            .insert("payload".into(), serde_json::json!({"opaque_key": 1}));
        validate_model(&model).unwrap();
        model.nodes[2].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
        model
            .variables
            .insert("results".into(), serde_json::json!([]));
        validate_model(&model).unwrap();
        model.nodes[2].repeat = None;
        model.nodes[1].repeat = Some(ProcessRepeatSpec::StructuredLoop {
            condition: "vars.more".into(),
            test_before: true,
            max_iterations: 3,
            output_collection_variable: "results".into(),
        });
        validate_model(&model).unwrap();
        model.nodes[1].repeat = None;
        if let ProcessNodeKind::SendTask { message_ref, .. } = &mut model.nodes[1].kind {
            *message_ref = "Missing".into();
        }
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("unknown declaration"));
    }

    #[test]
    fn send_boundaries_preserve_the_repeated_group_and_child_local_profiles() {
        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model
            .messages
            .push(tentaflow_protocol::processes::ProcessMessageDeclaration {
                message_id: "Message_1".into(),
                name: "order.received".into(),
            });
        model
            .variables
            .insert("case_key".into(), serde_json::json!("case-1"));
        model
            .variables
            .insert("payload".into(), serde_json::json!({"business_key": 1}));
        model.nodes.push(ProcessNode {
            activity_io: None,
            id: "Send_1".into(),
            name: "Admit".into(),
            repeat: None,
            kind: ProcessNodeKind::SendTask {
                message_ref: "Message_1".into(),
                target: ProcessMessageTargetSpec::Start {
                    definition_id: uuid::Uuid::nil().to_string(),
                    process_id: None,
                    start_node_id: None,
                },
                correlation_expression: "vars.case_key".into(),
                payload_expression: "vars.payload".into(),
                ttl_seconds: 60,
            },
        });
        model.nodes.push(ProcessNode {
            activity_io: None,
            id: "Boundary_Timer".into(),
            name: "Deadline".into(),
            repeat: None,
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Send_1".into(),
                cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 60 },
            },
        });
        model.nodes.push(ProcessNode {
            activity_io: None,
            id: "Boundary_Message".into(),
            name: "Reply".into(),
            repeat: None,
            kind: ProcessNodeKind::BoundaryMessage {
                attached_to_id: "Send_1".into(),
                cancel_activity: false,
                message_ref: "Message_1".into(),
                correlation_expression: "vars.case_key".into(),
                output_mapping: BTreeMap::new(),
            },
        });
        model.sequence_flows[0].target_id = "Send_1".into();
        for (id, source_id) in [
            ("Flow_Send", "Send_1"),
            ("Flow_Timer", "Boundary_Timer"),
            ("Flow_Message", "Boundary_Message"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                call_start_node_id: None,
                id: id.into(),
                source_id: source_id.into(),
                target_id: "End_1".into(),
                condition: None,
            });
        }
        validate_model(&model).unwrap();
        model.nodes[4].kind = ProcessNodeKind::BoundaryError {
            attached_to_id: "Send_1".into(),
            error_ref: None,
            output_mapping: BTreeMap::new(),
        };
        let error = validate_model(&model).unwrap_err();
        assert!(
            error.to_string().contains("unsupported attachment"),
            "{error:#}"
        );
        model.nodes[4].kind = ProcessNodeKind::BoundaryEscalation {
            attached_to_id: "Send_1".into(),
            escalation_ref: None,
            cancel_activity: false,
            output_mapping: BTreeMap::new(),
        };
        model.nodes.push(ProcessNode {
            activity_io: None,
            id: "Escalation_Wait".into(),
            name: "Review".into(),
            repeat: None,
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            },
        });
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "Flow_Message")
            .unwrap()
            .target_id = "Escalation_Wait".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_Escalation_Wait".into(),
            source_id: "Escalation_Wait".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        let error = validate_model(&model).unwrap_err();
        assert!(
            error.to_string().contains("unsupported attachment"),
            "{error:#}"
        );
        model.sequence_flows.pop();
        model
            .sequence_flows
            .iter_mut()
            .find(|flow| flow.id == "Flow_Message")
            .unwrap()
            .target_id = "End_1".into();
        model.nodes.pop();
        model.nodes[4].kind = ProcessNodeKind::BoundaryMessage {
            attached_to_id: "Send_1".into(),
            cancel_activity: false,
            message_ref: "Message_1".into(),
            correlation_expression: "vars.case_key".into(),
            output_mapping: BTreeMap::new(),
        };
        validate_model(&model).unwrap();
        model.nodes[2].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
        model
            .variables
            .insert("results".into(), serde_json::json!([]));
        validate_model(&model).unwrap();

        let mut embedded = embedded_model();
        embedded.timer_timezone = Some("UTC".into());
        embedded.messages = model.messages.clone();
        embedded.variables = model.variables.clone();
        if let ProcessNodeKind::SubProcess { body, .. } = &mut embedded.nodes[1].kind {
            body.nodes[1].kind = model.nodes[2].kind.clone();
            body.nodes[1].repeat = None;
            body.nodes[1].id = "Send_1".into();
            body.nodes.push(ProcessNode {
                activity_io: None,
                id: "Local_Timer".into(),
                name: "Deadline".into(),
                repeat: None,
                kind: ProcessNodeKind::BoundaryTimer {
                    attached_to_id: "Send_1".into(),
                    cancel_activity: true,
                    timer: ProcessTimerSpec::Duration { seconds: 60 },
                },
            });
            body.nodes.push(ProcessNode {
                activity_io: None,
                id: "Local_Message".into(),
                name: "Reply".into(),
                repeat: None,
                kind: ProcessNodeKind::BoundaryMessage {
                    attached_to_id: "Send_1".into(),
                    cancel_activity: false,
                    message_ref: "Message_1".into(),
                    correlation_expression: "vars.case_key".into(),
                    output_mapping: BTreeMap::new(),
                },
            });
            body.sequence_flows[0].target_id = "Send_1".into();
            body.sequence_flows[1].source_id = "Send_1".into();
            for (id, source_id) in [
                ("Local_Timer_Flow", "Local_Timer"),
                ("Local_Message_Flow", "Local_Message"),
            ] {
                body.sequence_flows.push(ProcessSequenceFlow {
                    call_start_node_id: None,
                    id: id.into(),
                    source_id: source_id.into(),
                    target_id: "LocalEnd".into(),
                    condition: None,
                });
            }
        }
        validate_model(&embedded).unwrap();
        if let ProcessNodeKind::SubProcess { body, .. } = &mut embedded.nodes[1].kind {
            body.variables
                .insert("results".into(), serde_json::json!([]));
            body.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
                mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
                input: ProcessMultiInstanceInput::Cardinality { count: 2 },
                output_collection_variable: "results".into(),
            });
        }
        validate_model(&embedded).unwrap();
    }

    #[test]
    fn manual_and_receive_waits_accept_group_and_child_local_timer_and_message_boundaries() {
        for kind in [
            ProcessNodeKind::ManualTask {
                assignee_user_id: None,
                instructions: "Complete the external check, then acknowledge it".into(),
            },
            ProcessNodeKind::ReceiveTask {
                message_ref: "Message_1".into(),
                correlation_expression: "vars.case_key".into(),
                output_mapping: BTreeMap::from([("received_payload".into(), "outputs".into())]),
            },
        ] {
            let mut model = starter_model();
            model.timer_timezone = Some("UTC".into());
            model
                .messages
                .push(tentaflow_protocol::processes::ProcessMessageDeclaration {
                    message_id: "Message_1".into(),
                    name: "order.received".into(),
                });
            model
                .variables
                .insert("case_key".into(), serde_json::json!("case-1"));
            model
                .variables
                .insert("received_payload".into(), serde_json::Value::Null);
            model
                .variables
                .insert("boundary_payload".into(), serde_json::Value::Null);
            model.nodes.push(ProcessNode {
                activity_io: None,
                id: "Wait_1".into(),
                name: "External wait".into(),
                repeat: None,
                kind: kind.clone(),
            });
            model.nodes.push(ProcessNode {
                activity_io: None,
                id: "Boundary_Timer".into(),
                name: "Deadline".into(),
                repeat: None,
                kind: ProcessNodeKind::BoundaryTimer {
                    attached_to_id: "Wait_1".into(),
                    cancel_activity: true,
                    timer: ProcessTimerSpec::Duration { seconds: 60 },
                },
            });
            model.nodes.push(ProcessNode {
                activity_io: None,
                id: "Boundary_Message".into(),
                name: "Reply".into(),
                repeat: None,
                kind: ProcessNodeKind::BoundaryMessage {
                    attached_to_id: "Wait_1".into(),
                    cancel_activity: false,
                    message_ref: "Message_1".into(),
                    correlation_expression: "vars.case_key".into(),
                    output_mapping: BTreeMap::from([("boundary_payload".into(), "outputs".into())]),
                },
            });
            model.sequence_flows[0].target_id = "Wait_1".into();
            for (id, source_id) in [
                ("Flow_Wait", "Wait_1"),
                ("Flow_Timer", "Boundary_Timer"),
                ("Flow_Message", "Boundary_Message"),
            ] {
                model.sequence_flows.push(ProcessSequenceFlow {
                    call_start_node_id: None,
                    id: id.into(),
                    source_id: source_id.into(),
                    target_id: "End_1".into(),
                    condition: None,
                });
            }
            validate_model(&model).unwrap();
            model.nodes[2].repeat = Some(ProcessRepeatSpec::MultiInstance {
                mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
                input: ProcessMultiInstanceInput::Cardinality { count: 2 },
                output_collection_variable: "results".into(),
            });
            model
                .variables
                .insert("results".into(), serde_json::json!([]));
            validate_model(&model).unwrap();
            model.nodes[2].repeat = None;
            model.nodes[3].kind = ProcessNodeKind::BoundaryError {
                attached_to_id: "Wait_1".into(),
                error_ref: None,
                output_mapping: BTreeMap::new(),
            };
            model.timer_timezone = None;
            validate_draft(&model).unwrap();
            let error = validate_model(&model).unwrap_err();
            assert!(
                error.to_string().contains("unsupported attachment"),
                "{error:#}"
            );

            let mut embedded = embedded_model();
            embedded.timer_timezone = Some("UTC".into());
            embedded.messages = model.messages.clone();
            embedded.variables = model.variables.clone();
            if let ProcessNodeKind::SubProcess { body, .. } = &mut embedded.nodes[1].kind {
                body.variables
                    .insert("case_key".into(), serde_json::json!("case-1"));
                body.variables
                    .insert("received_payload".into(), serde_json::Value::Null);
                body.variables
                    .insert("boundary_payload".into(), serde_json::Value::Null);
                body.nodes[1].kind = kind;
                body.nodes[1].id = "Local_Wait".into();
                body.nodes.push(ProcessNode {
                    activity_io: None,
                    id: "Local_Timer".into(),
                    name: "Deadline".into(),
                    repeat: None,
                    kind: ProcessNodeKind::BoundaryTimer {
                        attached_to_id: "Local_Wait".into(),
                        cancel_activity: true,
                        timer: ProcessTimerSpec::Duration { seconds: 60 },
                    },
                });
                body.nodes.push(ProcessNode {
                    activity_io: None,
                    id: "Local_Message".into(),
                    name: "Reply".into(),
                    repeat: None,
                    kind: ProcessNodeKind::BoundaryMessage {
                        attached_to_id: "Local_Wait".into(),
                        cancel_activity: false,
                        message_ref: "Message_1".into(),
                        correlation_expression: "vars.case_key".into(),
                        output_mapping: BTreeMap::from([(
                            "boundary_payload".into(),
                            "outputs".into(),
                        )]),
                    },
                });
                body.sequence_flows[0].target_id = "Local_Wait".into();
                body.sequence_flows[1].source_id = "Local_Wait".into();
                for (id, source_id) in [
                    ("Local_Timer_Flow", "Local_Timer"),
                    ("Local_Message_Flow", "Local_Message"),
                ] {
                    body.sequence_flows.push(ProcessSequenceFlow {
                        call_start_node_id: None,
                        id: id.into(),
                        source_id: source_id.into(),
                        target_id: "LocalEnd".into(),
                        condition: None,
                    });
                }
            }
            validate_model(&embedded).unwrap();
        }
    }

    #[test]
    fn signals_require_exact_namespace_declarations_and_exclude_direct_repeat() {
        let mut model = starter_model();
        model.target_namespace = Some("urn:orders".into());
        model
            .signals
            .push(tentaflow_protocol::processes::ProcessSignalDeclaration {
                signal_id: "Signal_1".into(),
                namespace_uri: "urn:orders".into(),
                name: "Order changed".into(),
            });
        model
            .variables
            .insert("payload".into(), serde_json::json!({"business_key": 1}));
        model.nodes.insert(
            1,
            ProcessNode {
                activity_io: None,
                id: "Throw_1".into(),
                name: "Admit signal".into(),
                repeat: None,
                kind: ProcessNodeKind::SignalThrow {
                    signal_ref: "Signal_1".into(),
                    payload_expression: "vars.payload".into(),
                    ttl_seconds: 60,
                },
            },
        );
        model.nodes.insert(
            2,
            ProcessNode {
                activity_io: None,
                id: "Catch_1".into(),
                name: "Wait for signal".into(),
                repeat: None,
                kind: ProcessNodeKind::SignalCatch {
                    signal_ref: "Signal_1".into(),
                    output_mapping: BTreeMap::new(),
                },
            },
        );
        model.sequence_flows[0].target_id = "Throw_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_Throw".into(),
            source_id: "Throw_1".into(),
            target_id: "Catch_1".into(),
            condition: None,
        });
        model.sequence_flows.push(ProcessSequenceFlow {
            call_start_node_id: None,
            id: "Flow_Catch".into(),
            source_id: "Catch_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        validate_model(&model).unwrap();
        model.target_namespace = None;
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("explicit target namespace"));
        model.target_namespace = Some("urn:orders".into());
        model.signals[0].namespace_uri = "urn:foreign".into();
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("namespace differs"));
        model.signals[0].namespace_uri = "urn:orders".into();
        model.nodes[2].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
        assert!(validate_draft(&model)
            .unwrap_err()
            .to_string()
            .contains("requires a supported activity"));
        model.nodes[2].repeat = None;
        if let ProcessNodeKind::SignalThrow { ttl_seconds, .. } = &mut model.nodes[1].kind {
            *ttl_seconds = 0;
        }
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("TTL"));
    }

    #[test]
    fn draft_refuses_more_executable_bodies_than_xml_can_import() {
        use tentaflow_protocol::processes::ProcessExecutableProcess;
        let mut model = starter_model();
        model.additional_processes = (2..=17)
            .map(|number| ProcessExecutableProcess {
                process_id: format!("Process_{number}"),
                process_name: None,
                nodes: Vec::new(),
                sequence_flows: Vec::new(),
                variables: BTreeMap::new(),
                diagram: ProcessDiagram::default(),
                timer_timezone: None,
                work_calendar: None,
                calendar_pin: None,
                modeling: None,
            })
            .collect();
        assert!(validate_draft(&model)
            .unwrap_err()
            .to_string()
            .contains("too many executable process bodies"));
    }

    #[test]
    fn selected_body_and_local_call_pin_the_exact_document_process_and_start() {
        use tentaflow_protocol::processes::{
            ProcessCallActivity, ProcessCallableReference, ProcessExecutableProcess,
        };
        let mut model = starter_model();
        let called = ProcessExecutableProcess {
            process_id: "Process_2".into(),
            process_name: Some(String::new()),
            nodes: vec![
                ProcessNode {
                    id: "Start_2".into(),
                    name: "First".into(),
                    kind: ProcessNodeKind::Start,
                    repeat: None,
                    activity_io: None,
                },
                ProcessNode {
                    id: "End_2".into(),
                    name: "Done".into(),
                    kind: ProcessNodeKind::End,
                    repeat: None,
                    activity_io: None,
                },
            ],
            sequence_flows: vec![ProcessSequenceFlow {
                id: "Flow_2".into(),
                source_id: "Start_2".into(),
                target_id: "End_2".into(),
                condition: None,
                call_start_node_id: None,
            }],
            variables: BTreeMap::new(),
            diagram: ProcessDiagram::default(),
            timer_timezone: None,
            work_calendar: None,
            calendar_pin: None,
            modeling: None,
        };
        model.additional_processes.push(called);
        model.nodes.insert(
            1,
            ProcessNode {
                id: "Call_1".into(),
                name: "Call local".into(),
                kind: ProcessNodeKind::CallActivity(ProcessCallActivity {
                    target: ProcessCallTarget::LocalBody {
                        called_element: ProcessCallableReference {
                            namespace_uri: "https://tentaflow.app/bpmn/1".into(),
                            process_id: "Process_2".into(),
                        },
                    },
                    input_mapping: BTreeMap::new(),
                    output_mapping: BTreeMap::new(),
                }),
                repeat: None,
                activity_io: None,
            },
        );
        model.sequence_flows[0].target_id = "Call_1".into();
        model.sequence_flows[0].call_start_node_id = Some("Start_2".into());
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "AfterCall".into(),
            source_id: "Call_1".into(),
            target_id: "End_1".into(),
            condition: None,
            call_start_node_id: None,
        });
        validate_model(&model).unwrap();
        let body = selected_body(&model, "Process_2", &[]).unwrap();
        assert_eq!(body.nodes[0].id, "Start_2");
        assert_eq!(body.sequence_flows[0].id, "Flow_2");
        assert!(selected_body(&model, "missing", &[]).is_err());
        if let ProcessNodeKind::CallActivity(call) = &mut model.nodes[1].kind {
            if let ProcessCallTarget::LocalBody { called_element } = &mut call.target {
                called_element.process_id = "Process_1".into();
            }
        }
        model.sequence_flows[0].call_start_node_id = Some("Start_1".into());
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("local Call body cycle"));
        if let ProcessNodeKind::CallActivity(call) = &mut model.nodes[1].kind {
            if let ProcessCallTarget::LocalBody { called_element } = &mut call.target {
                called_element.process_id = "Process_2".into();
            }
        }
        model.sequence_flows[0].call_start_node_id = Some("Start_1".into());
        assert!(validate_model(&model)
            .unwrap_err()
            .to_string()
            .contains("no None Start"));
        model.sequence_flows[0].call_start_node_id = Some("Start_2".into());
        model.additional_processes[0].nodes[0].id = "Start_1".into();
        assert!(format!("{:#}", validate_draft(&model).unwrap_err())
            .contains("duplicate BPMN ID"));
    }
}
