// ============ File: bpmn.rs — bounded BPMN B1 XML import and export ============

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{bail, ensure, Context, Result};
use quick_xml::events::Event;
use quick_xml::name::{QName, ResolveResult};
use quick_xml::{NsReader, XmlVersion};
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessCalendarPin, ProcessCallableReference, ProcessDiagnostic, ProcessDiagram, ProcessEdgeDiagram, ProcessModel,
    ProcessErrorDeclaration, ProcessMessageDeclaration, ProcessMessageTargetSpec, ProcessNode,
    ProcessNodeKind, ProcessPoint, ProcessSequenceFlow, ProcessShape, ProcessTimerSpec,
    ProcessSubProcess, ProcessWorkCalendar,
};

use super::model::{validate_model, validate_timer_spec, validate_variables, MAX_MODEL_BYTES, MAX_VARIABLE_BYTES};

const BPMN: &str = "http://www.omg.org/spec/BPMN/20100524/MODEL";
const BPMNDI: &str = "http://www.omg.org/spec/BPMN/20100524/DI";
const DC: &str = "http://www.omg.org/spec/DD/20100524/DC";
const DI: &str = "http://www.omg.org/spec/DD/20100524/DI";
const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const TF: &str = "https://tentaflow.app/bpmn/1";
const GRAPH_ELEMENTS: [(&str, &str); 14] = [
    (BPMN, "extensionElements"), (BPMN, "startEvent"),
    (BPMN, "intermediateCatchEvent"), (BPMN, "intermediateThrowEvent"),
    (BPMN, "boundaryEvent"), (BPMN, "endEvent"), (BPMN, "userTask"),
    (BPMN, "serviceTask"), (BPMN, "subProcess"), (BPMN, "callActivity"),
    (BPMN, "exclusiveGateway"), (BPMN, "eventBasedGateway"),
    (BPMN, "parallelGateway"), (BPMN, "sequenceFlow"),
];

#[derive(Debug)]
struct Element {
    ns: String,
    local: String,
    attrs: HashMap<(String, String), String>,
    qnames: HashMap<String, (String, String)>,
    children: Vec<Element>,
    text: String,
    offset: usize,
}

#[derive(Debug)]
struct XmlElementError {
    message: String,
    element_id: Option<String>,
    offset: usize,
}

impl std::fmt::Display for XmlElementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for XmlElementError {}

impl Element {
    fn is(&self, ns: &str, local: &str) -> bool {
        self.ns == ns && self.local == local
    }
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .get(&(String::new(), name.to_string()))
            .map(String::as_str)
    }
    fn required(&self, name: &str) -> Result<String> {
        self.attr(name).map(str::to_string).with_context(|| {
            format!(
                "{} at byte {} requires attribute {name}",
                self.local, self.offset
            )
        })
    }
    fn reference(&self, name: &str, namespace: &str) -> Result<String> {
        let (uri, local) = self.qnames.get(name).ok_or_else(|| XmlElementError {
            message: format!("{} at byte {} requires QName {name}", self.local, self.offset),
            element_id: self.attr("id").map(str::to_string),
            offset: self.offset,
        })?;
        if !uri.is_empty() && uri != namespace {
            return Err(XmlElementError {
                message: format!("foreign QName {name} at byte {}", self.offset),
                element_id: self.attr("id").map(str::to_string),
                offset: self.offset,
            }.into());
        }
        Ok(local.clone())
    }
    fn attrs_only(&self, allowed: &[&str]) -> Result<()> {
        for (ns, name) in self.attrs.keys() {
            ensure!(
                (ns.is_empty() && allowed.contains(&name.as_str()))
                    || (self.is(BPMN, "conditionExpression") && ns == XSI && name == "type"),
                "unsupported {} attribute {{{}}}{} at byte {}",
                self.local,
                ns,
                name,
                self.offset
            );
        }
        Ok(())
    }
    fn children_only(&self, allowed: &[(&str, &str)]) -> Result<()> {
        for child in &self.children {
            ensure!(
                allowed.iter().any(|(ns, local)| child.is(ns, local)),
                "unsupported element {{{}}}{} at byte {}",
                child.ns,
                child.local,
                child.offset
            );
        }
        Ok(())
    }
    fn child(&self, ns: &str, local: &str) -> Result<Option<&Element>> {
        let mut matches = self.children.iter().filter(|child| child.is(ns, local));
        let first = matches.next();
        if let Some(duplicate) = matches.next() {
            return Err(XmlElementError {
                message: format!(
                    "duplicate element {{{ns}}}{local} at byte {} inside {}",
                    duplicate.offset, self.local
                ),
                element_id: self.attr("id").map(str::to_string),
                offset: duplicate.offset,
            }
            .into());
        }
        Ok(first)
    }
}

fn ns_text(result: ResolveResult<'_>) -> Result<String> {
    match result {
        ResolveResult::Bound(namespace) => Ok(String::from_utf8(namespace.0.to_vec())?),
        ResolveResult::Unbound => Ok(String::new()),
        ResolveResult::Unknown(prefix) => bail!(
            "undeclared XML namespace prefix {}",
            String::from_utf8_lossy(&prefix)
        ),
    }
}

fn parse_tree(xml: &str) -> Result<Element> {
    ensure!(xml.len() <= MAX_MODEL_BYTES, "BPMN XML exceeds 512 KiB");
    let mut reader = NsReader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Element> = Vec::new();
    let mut root = None;
    let mut xml_ids = HashMap::new();
    let mut buffer = Vec::new();
    loop {
        let offset = reader.buffer_position() as usize;
        let (namespace, event) = reader.read_resolved_event_into(&mut buffer)?;
        let is_empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(start) | Event::Empty(start) => {
                let ns = ns_text(namespace)?;
                let local = String::from_utf8(start.local_name().as_ref().to_vec())?;
                let mut attrs = HashMap::new();
                let mut qnames = HashMap::new();
                for attr in start.attributes().with_checks(true) {
                    let attr = attr?;
                    let raw = attr.key.as_ref();
                    if raw == b"xmlns" || raw.starts_with(b"xmlns:") {
                        continue;
                    }
                    let (attr_ns, attr_local) = reader.resolver().resolve_attribute(attr.key);
                    let attr_ns = ns_text(attr_ns)?;
                    let attr_local = String::from_utf8(attr_local.as_ref().to_vec())?;
                    let value = attr.normalized_value(XmlVersion::Implicit1_0)?.into_owned();
                    if attr_ns.is_empty() && matches!(attr_local.as_str(), "messageRef" | "errorRef" | "calledElement") {
                        let invalid_qname = |reason: &str| XmlElementError {
                            message: format!("{reason} at byte {offset}"),
                            element_id: stack.last().and_then(|parent| parent.attr("id")).map(str::to_string),
                            offset,
                        };
                        let (uri, local) = if let Some((prefix, local)) = value.split_once(':') {
                            if prefix.is_empty() || local.is_empty() || local.contains(':') {
                                return Err(invalid_qname("malformed QName").into());
                            }
                            let (resolved, _) = reader.resolver().resolve_element(QName(value.as_bytes()));
                            (ns_text(resolved).map_err(|_| invalid_qname("undeclared QName namespace"))?, local.to_string())
                        } else {
                            (String::new(), value.clone())
                        };
                        if local.is_empty() || !local.bytes().next().is_some_and(|first| first.is_ascii_alphabetic())
                            || !local.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')) {
                            return Err(invalid_qname("invalid QName").into());
                        }
                        qnames.insert(attr_local.clone(), (uri, local));
                    }
                    if attr_ns == XSI && attr_local == "type" {
                        let (type_ns, type_local) =
                            reader.resolver().resolve_element(QName(value.as_bytes()));
                        ensure!(
                            ns_text(type_ns)? == BPMN
                                && type_local.as_ref() == b"tFormalExpression",
                            "unsupported condition expression type at byte {offset}"
                        );
                    }
                    ensure!(
                        attrs.insert((attr_ns, attr_local), value).is_none(),
                        "duplicate XML attribute at byte {offset}"
                    );
                }
                if [BPMN, BPMNDI, DC, DI].contains(&ns.as_str()) {
                    if let Some(id) = attrs.get(&(String::new(), "id".to_string())) {
                        if let Some(first_offset) = xml_ids.insert(id.clone(), offset) {
                            return Err(XmlElementError {
                                message: format!("duplicate XML ID {id} at byte {offset}; first at byte {first_offset}"),
                                element_id: Some(id.clone()),
                                offset,
                            }
                            .into());
                        }
                    }
                }
                let element = Element {
                    ns,
                    local,
                    attrs,
                    qnames,
                    children: Vec::new(),
                    text: String::new(),
                    offset,
                };
                if is_empty {
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(element);
                    } else {
                        ensure!(root.is_none(), "multiple XML roots");
                        root = Some(element);
                    }
                } else {
                    stack.push(element);
                }
            }
            Event::End(_) => {
                let element = stack.pop().context("unexpected XML closing tag")?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(element);
                } else {
                    ensure!(root.is_none(), "multiple XML roots");
                    root = Some(element);
                }
            }
            Event::Text(text) => {
                let decoded = text.xml_content(XmlVersion::Implicit1_0)?;
                let value = quick_xml::escape::unescape(&decoded)?;
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&value);
                } else {
                    ensure!(value.trim().is_empty(), "text outside BPMN root");
                }
            }
            Event::CData(text) => {
                let value = text.decode()?;
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&value);
                } else {
                    ensure!(value.trim().is_empty(), "text outside BPMN root");
                }
            }
            Event::GeneralRef(reference) => {
                let value = if let Some(character) = reference.resolve_char_ref()? {
                    character.to_string()
                } else {
                    let name = reference.decode()?;
                    quick_xml::escape::resolve_predefined_entity(&name)
                        .context("custom XML entities are unsupported")?
                        .to_string()
                };
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&value);
                } else {
                    ensure!(
                        value.trim().is_empty(),
                        "character reference outside BPMN root"
                    );
                }
            }
            Event::Decl(_) | Event::Comment(_) => {}
            Event::Eof => break,
            Event::DocType(_) | Event::PI(_) => {
                bail!("unsupported XML directive or entity at byte {offset}")
            }
        }
        buffer.clear();
    }
    ensure!(stack.is_empty(), "unclosed XML element");
    root.context("BPMN XML is empty")
}

fn mapping(parent: &Element, name: &str) -> Result<BTreeMap<String, String>> {
    match parent.child(TF, name)? {
        Some(element) => {
            element.attrs_only(&[])?;
            ensure!(
                element.children.is_empty(),
                "mapping element may only contain JSON text"
            );
            serde_json::from_str(element.text.trim())
                .with_context(|| format!("invalid {name} JSON at byte {}", element.offset))
        }
        None => Ok(BTreeMap::new()),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageStartConfig {
    output_mapping: BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageCatchConfig {
    correlation_expression: String,
    output_mapping: BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageThrowConfig {
    target: ProcessMessageTargetSpec,
    correlation_expression: String,
    payload_expression: String,
    ttl_seconds: u32,
}

fn message_configuration<T: serde::de::DeserializeOwned>(element: &Element) -> Result<T> {
    let extension = element.child(BPMN, "extensionElements")?
        .context("message event requires TentaFlow extension")?;
    extension.attrs_only(&[])?;
    extension.children_only(&[(TF, "message")])?;
    ensure!(extension.children.len() == 1, "message event requires exactly one TentaFlow message config");
    let config = extension.child(TF, "message")?.expect("validated message extension");
    config.attrs_only(&[])?;
    ensure!(config.children.is_empty(), "message config must contain JSON text");
    serde_json::from_str(config.text.trim()).map_err(|error| XmlElementError {
        message: format!("invalid message config at byte {}: {error}", config.offset),
        element_id: element.attr("id").map(str::to_string),
        offset: config.offset,
    }.into())
}

fn event_reference(element: &Element, kind: &str, namespace: &str) -> Result<String> {
    let definition = element.child(BPMN, kind)?.context("event definition is required")?;
    let attribute = if kind == "messageEventDefinition" { "messageRef" } else { "errorRef" };
    definition.attrs_only(&[attribute])?;
    ensure!(definition.children.is_empty() && definition.text.trim().is_empty(),
        "event definition cannot contain other content at byte {}", definition.offset);
    definition.reference(attribute, namespace)
}

fn process_configuration(
    process: &Element,
    process_id: &str,
) -> Result<(BTreeMap<String, serde_json::Value>, Option<String>, Option<ProcessWorkCalendar>, Option<ProcessCalendarPin>)> {
    let Some(extension) = process.child(BPMN, "extensionElements")? else {
        return Ok((BTreeMap::new(), None, None, None));
    };
    extension.attrs_only(&[])?;
    extension.children_only(&[(TF, "variables"), (TF, "timerTimezone"), (TF, "workCalendar"), (TF, "calendarPin")])?;
    let timer_timezone = extension.child(TF, "timerTimezone")?
        .map(|element| {
            element.attrs_only(&[])?;
            ensure!(element.children.is_empty(), "timerTimezone must be text at byte {}", element.offset);
            ensure!(!element.text.is_empty(), "timerTimezone is empty at byte {}", element.offset);
            Ok(element.text.clone())
        }).transpose()?;
    let variables = extension
        .child(TF, "variables")?
        .context("process extension requires TentaFlow variables")?;
    variables.attrs_only(&[])?;
    ensure!(
        variables.children.is_empty(),
        "TentaFlow process variables cannot contain XML elements at byte {}",
        variables.offset
    );
    let invalid = |reason: String| XmlElementError {
        message: format!(
            "invalid TentaFlow process variables at byte {}: {reason}",
            variables.offset
        ),
        element_id: Some(process_id.to_string()),
        offset: variables.offset,
    };
    if variables.text.len() > MAX_VARIABLE_BYTES {
        return Err(invalid("JSON text exceeds 256 KiB".into()).into());
    }
    let value: serde_json::Value =
        serde_json::from_str(variables.text.trim()).map_err(|error| invalid(error.to_string()))?;
    validate_variables(&value).map_err(|error| invalid(error.to_string()))?;
    let variables = serde_json::from_value(value).map_err(|error| invalid(error.to_string()))?;
    let work_calendar = extension.child(TF, "workCalendar")?.map(|element| -> Result<ProcessWorkCalendar> {
        element.attrs_only(&[])?;
        ensure!(element.children.is_empty(), "workCalendar must contain JSON text at byte {}", element.offset);
        serde_json::from_str::<ProcessWorkCalendar>(element.text.trim()).map_err(|error| XmlElementError {
            message: format!("invalid workCalendar at byte {}: {error}", element.offset),
            element_id: Some(process_id.to_string()), offset: element.offset,
        }.into())
    }).transpose()?;
    let calendar_pin = extension.child(TF, "calendarPin")?.map(|element| -> Result<ProcessCalendarPin> {
        element.attrs_only(&[])?;
        ensure!(element.children.is_empty(), "calendarPin must contain JSON text at byte {}", element.offset);
        serde_json::from_str::<ProcessCalendarPin>(element.text.trim()).map_err(|error| XmlElementError {
            message: format!("invalid calendarPin at byte {}: {error}", element.offset),
            element_id: Some(process_id.to_string()), offset: element.offset,
        }.into())
    }).transpose()?;
    Ok((variables, timer_timezone, work_calendar, calendar_pin))
}

fn timer_spec(element: &Element, node_id: &str) -> Result<ProcessTimerSpec> {
    let timer = element.child(BPMN, "timerEventDefinition")?
        .context("timer event requires timerEventDefinition")?;
    timer.attrs_only(&[])?;
    timer.children_only(&[(BPMN, "timeDate"), (BPMN, "timeDuration"), (BPMN, "timeCycle"), (BPMN, "extensionElements")])?;
    ensure!(timer.children.len() == 1, "timer event {node_id} requires exactly one timer rule at byte {}", timer.offset);
    let rule = &timer.children[0];
    let invalid = |reason: String| XmlElementError {
        message: format!("invalid timer rule at byte {}: {reason}", rule.offset),
        element_id: Some(node_id.to_string()), offset: rule.offset,
    };
    if rule.is(BPMN, "extensionElements") {
        rule.attrs_only(&[])?;
        rule.children_only(&[(TF, "dailyTimer"), (TF, "workingDuration")])?;
        ensure!(rule.children.len() == 1, "timer extension requires one timer rule at byte {}", rule.offset);
        if let Some(working) = rule.child(TF, "workingDuration")? {
            working.attrs_only(&["seconds"])?;
            ensure!(working.children.is_empty() && working.text.trim().is_empty(), "workingDuration must have only attributes at byte {}", working.offset);
            return Ok(ProcessTimerSpec::WorkingDuration {
                seconds: working.required("seconds")?.parse().map_err(|error| invalid(format!("invalid working seconds: {error}")))?,
            });
        }
        let daily = rule.child(TF, "dailyTimer")?.context("dailyTimer is required")?;
        daily.attrs_only(&["hour", "minute", "totalFirings"])?;
        ensure!(daily.children.is_empty() && daily.text.trim().is_empty(), "dailyTimer must have only attributes at byte {}", daily.offset);
        return Ok(ProcessTimerSpec::Daily {
            hour: daily.required("hour")?.parse().map_err(|error| invalid(format!("invalid hour: {error}")))?,
            minute: daily.required("minute")?.parse().map_err(|error| invalid(format!("invalid minute: {error}")))?,
            total_firings: daily.attr("totalFirings").map(|count| count.parse().map_err(|error| invalid(format!("invalid count: {error}")))).transpose()?,
        });
    }
    rule.attrs_only(&[])?;
    ensure!(rule.children.is_empty(), "timer rule must be text at byte {}", rule.offset);
    let value = rule.text.trim();
    if rule.is(BPMN, "timeDate") {
        return Ok(ProcessTimerSpec::Date { at: value.to_string() });
    }
    let parse_seconds = |value: &str| -> Result<u32> {
        let profile = value.strip_prefix('P').ok_or_else(|| invalid("duration must begin with P".into()))?;
        let (days, time) = if let Some((days, time)) = profile.split_once('T') { (days, Some(time)) } else { (profile, None) };
        let mut seconds = 0_u64;
        let mut found = false;
        if !days.is_empty() {
            let day = days.strip_suffix('D').ok_or_else(|| invalid("unsupported duration day component".into()))?;
            ensure!(!day.is_empty() && day.bytes().all(|byte| byte.is_ascii_digit()), "duration days must be an integer");
            seconds = day.parse::<u64>()?.checked_mul(86_400).context("duration exceeds supported range")?;
            found = true;
        }
        if let Some(mut rest) = time {
            ensure!(!rest.is_empty(), "duration T must have time components");
            for (suffix, multiplier) in [('H', 3600_u64), ('M', 60), ('S', 1)] {
                if let Some(index) = rest.find(suffix) {
                    let amount = &rest[..index];
                    ensure!(!amount.is_empty() && amount.bytes().all(|byte| byte.is_ascii_digit()), "duration components must be integers in D/H/M/S order");
                    seconds = seconds.checked_add(amount.parse::<u64>()?.checked_mul(multiplier).context("duration exceeds supported range")?).context("duration exceeds supported range")?;
                    rest = &rest[index + 1..];
                    found = true;
                }
            }
            ensure!(rest.is_empty(), "unsupported duration component");
        }
        ensure!(found, "duration must contain a D/H/M/S component");
        u32::try_from(seconds).map_err(|error| invalid(format!("duration exceeds supported range: {error}")).into())
    };
    if rule.is(BPMN, "timeDuration") {
        return Ok(ProcessTimerSpec::Duration { seconds: parse_seconds(value)? });
    }
    let (repetition, duration) = value.split_once('/').ok_or_else(|| invalid("supported cycle profile is R[n]/PT{seconds}S".into()))?;
    ensure!(repetition.starts_with('R'), "timer cycle must start with R at byte {}", rule.offset);
    let count = &repetition[1..];
    let total_firings = if count.is_empty() { None } else {
        ensure!(count.bytes().all(|byte| byte.is_ascii_digit()), "timer cycle count must be an integer");
        Some(count.parse().map_err(|error| invalid(format!("invalid count: {error}")))?)
    };
    let seconds = duration.strip_prefix("PT").and_then(|value| value.strip_suffix('S'))
        .ok_or_else(|| invalid("supported cycle profile is R[n]/PT{seconds}S".into()))?;
    ensure!(!seconds.is_empty() && seconds.bytes().all(|byte| byte.is_ascii_digit()),
        "timer cycle requires integer seconds at byte {}", rule.offset);
    Ok(ProcessTimerSpec::Cycle {
        seconds: seconds.parse().map_err(|error| invalid(format!("invalid cycle seconds: {error}")))?,
        total_firings,
    })
}

fn node_from_xml(element: &Element, target_namespace: &str) -> Result<ProcessNode> {
    let id = element.required("id")?;
    let name = element.attr("name").unwrap_or_default().to_string();
    let parsed_timer = || timer_spec(element, &id).map_err(|error| {
        if error.downcast_ref::<XmlElementError>().is_some() { error }
        else { XmlElementError {
            message: format!("invalid timer rule at byte {}: {error}", element.offset),
            element_id: Some(id.clone()), offset: element.offset,
        }.into() }
    });
    let kind = match element.local.as_str() {
        "startEvent" => {
            element.attrs_only(&["id", "name"])?;
            element.children_only(&[(BPMN, "timerEventDefinition"), (BPMN, "messageEventDefinition"), (BPMN, "extensionElements")])?;
            if element.children.is_empty() { ProcessNodeKind::Start }
            else if element.child(BPMN, "timerEventDefinition")?.is_some() {
                ensure!(element.children.len() == 1, "timer start cannot have another event definition");
                ProcessNodeKind::TimerStart { timer: parsed_timer()? }
            } else {
                ensure!(element.children.len() == 2, "message start requires one definition and one config");
                let config: MessageStartConfig = message_configuration(element)?;
                ProcessNodeKind::MessageStart {
                    message_ref: event_reference(element, "messageEventDefinition", target_namespace)?,
                    output_mapping: config.output_mapping,
                }
            }
        }
        "intermediateCatchEvent" => {
            element.attrs_only(&["id", "name"])?;
            element.children_only(&[(BPMN, "timerEventDefinition"), (BPMN, "messageEventDefinition"), (BPMN, "extensionElements")])?;
            if element.child(BPMN, "timerEventDefinition")?.is_some() {
                ensure!(element.children.len() == 1, "timer catch cannot have another event definition");
                ProcessNodeKind::TimerCatch { timer: parsed_timer()? }
            } else {
                ensure!(element.children.len() == 2, "message catch requires one definition and one config");
                let config: MessageCatchConfig = message_configuration(element)?;
                ProcessNodeKind::MessageCatch {
                    message_ref: event_reference(element, "messageEventDefinition", target_namespace)?,
                    correlation_expression: config.correlation_expression,
                    output_mapping: config.output_mapping,
                }
            }
        }
        "intermediateThrowEvent" => {
            element.attrs_only(&["id", "name"])?;
            element.children_only(&[(BPMN, "messageEventDefinition"), (BPMN, "extensionElements")])?;
            ensure!(element.children.len() == 2, "message throw requires one definition and one config");
            let config: MessageThrowConfig = message_configuration(element)?;
            ProcessNodeKind::MessageThrow {
                message_ref: event_reference(element, "messageEventDefinition", target_namespace)?,
                target: config.target,
                correlation_expression: config.correlation_expression,
                payload_expression: config.payload_expression,
                ttl_seconds: config.ttl_seconds,
            }
        }
        "boundaryEvent" => {
            element.attrs_only(&["id", "name", "attachedToRef", "cancelActivity"])?;
            element.children_only(&[(BPMN, "timerEventDefinition"), (BPMN, "messageEventDefinition"), (BPMN, "errorEventDefinition"), (BPMN, "extensionElements")])?;
            let cancel_activity = match element.attr("cancelActivity") {
                None | Some("true" | "1") => true,
                Some("false" | "0") => false,
                Some(other) => return Err(XmlElementError {
                    message: format!("invalid boundary cancelActivity {other} at byte {}", element.offset),
                    element_id: Some(id.clone()),
                    offset: element.offset,
                }.into()),
            };
            if element.child(BPMN, "timerEventDefinition")?.is_some() {
                ensure!(element.children.len() == 1, "boundary timer cannot have another event definition");
                ProcessNodeKind::BoundaryTimer {
                    attached_to_id: element.required("attachedToRef")?,
                    cancel_activity,
                    timer: parsed_timer()?,
                }
            } else if element.child(BPMN, "messageEventDefinition")?.is_some() {
                ensure!(element.children.len() == 2, "boundary message requires one definition and one config");
                let config: MessageCatchConfig = message_configuration(element)?;
                ProcessNodeKind::BoundaryMessage {
                    attached_to_id: element.required("attachedToRef")?, cancel_activity,
                    message_ref: event_reference(element, "messageEventDefinition", target_namespace)?,
                    correlation_expression: config.correlation_expression,
                    output_mapping: config.output_mapping,
                }
            } else {
                ensure!(cancel_activity, "boundary error must interrupt its activity");
                ensure!(element.child(BPMN, "timerEventDefinition")?.is_none()
                    && element.child(BPMN, "messageEventDefinition")?.is_none(),
                    "boundary error cannot contain another event definition");
                let definition = element.child(BPMN, "errorEventDefinition")?
                    .context("boundary error requires errorEventDefinition")?;
                definition.attrs_only(&["errorRef"])?;
                ensure!(definition.children.is_empty() && definition.text.trim().is_empty(), "error definition must be empty");
                ensure!(element.children.len() <= 2, "boundary error has unsupported event definitions");
                let output_mapping = if let Some(extension) = element.child(BPMN, "extensionElements")? {
                    extension.attrs_only(&[])?;
                    extension.children_only(&[(TF, "outputMapping")])?;
                    ensure!(extension.children.len() == 1, "boundary error extension requires outputMapping");
                    mapping(extension, "outputMapping")?
                } else { BTreeMap::new() };
                ProcessNodeKind::BoundaryError {
                    attached_to_id: element.required("attachedToRef")?,
                    error_ref: if definition.attr("errorRef").is_some() {
                        Some(definition.reference("errorRef", target_namespace)?)
                    } else { None },
                    output_mapping,
                }
            }
        }
        "endEvent" => {
            element.attrs_only(&["id", "name"])?;
            element.children_only(&[(BPMN, "errorEventDefinition")])?;
            if element.child(BPMN, "errorEventDefinition")?.is_some() {
                ensure!(element.children.len() == 1, "error end requires one error definition");
                ProcessNodeKind::ErrorEnd {
                    error_ref: event_reference(element, "errorEventDefinition", target_namespace)?,
                }
            } else {
                ProcessNodeKind::End
            }
        }
        "parallelGateway" => {
            element.attrs_only(&["id", "name", "gatewayDirection"])?;
            element.children_only(&[])?;
            ProcessNodeKind::ParallelGateway
        }
        "exclusiveGateway" => {
            element.attrs_only(&["id", "name", "default", "gatewayDirection"])?;
            element.children_only(&[])?;
            ProcessNodeKind::ExclusiveGateway {
                default_flow_id: element.attr("default").map(str::to_string),
            }
        }
        "eventBasedGateway" => {
            element.attrs_only(&["id", "name", "gatewayDirection", "eventGatewayType", "instantiate"])?;
            element.children_only(&[])?;
            ensure!(element.attr("gatewayDirection").is_none_or(|value| value == "Diverging")
                && element.attr("eventGatewayType").is_none_or(|value| value == "Exclusive")
                && element.attr("instantiate").is_none_or(|value| value == "false"),
                "unsupported event gateway profile at byte {}", element.offset);
            ProcessNodeKind::EventBasedGateway
        }
        "userTask" => {
            element.attrs_only(&["id", "name"])?;
            element.children_only(&[(BPMN, "extensionElements")])?;
            let ext = element.child(BPMN, "extensionElements")?;
            let user = if let Some(ext) = ext {
                ext.attrs_only(&[])?;
                ext.children_only(&[(TF, "user")])?;
                ensure!(
                    ext.children.len() == 1,
                    "user task requires one TentaFlow extension"
                );
                ext.child(TF, "user")?
            } else {
                None
            };
            let (assignee_user_id, output_mapping) = if let Some(user) = user {
                user.attrs_only(&["assigneeUserId"])?;
                user.children_only(&[(TF, "outputMapping")])?;
                (
                    user.attr("assigneeUserId")
                        .filter(|id| !id.is_empty())
                        .map(str::to_string),
                    mapping(user, "outputMapping")?,
                )
            } else {
                (None, BTreeMap::new())
            };
            ProcessNodeKind::UserTask {
                assignee_user_id,
                output_mapping,
            }
        }
        "serviceTask" => {
            element.attrs_only(&["id", "name"])?;
            element.children_only(&[(BPMN, "extensionElements")])?;
            let ext = element
                .child(BPMN, "extensionElements")?
                .context("service task requires TentaFlow extension")?;
            ext.attrs_only(&[])?;
            ext.children_only(&[(TF, "service")])?;
            ensure!(
                ext.children.len() == 1,
                "service task requires exactly one TentaFlow service extension"
            );
            let service = ext
                .child(TF, "service")?
                .expect("validated service extension");
            service.attrs_only(&["flowId", "timeoutSeconds", "verification"])?;
            service.children_only(&[
                (TF, "inputMapping"),
                (TF, "outputMapping"),
                (TF, "condition"),
                (TF, "resultExpression"),
            ])?;
            let result_expression = service.child(TF, "resultExpression")?.map(|result| -> Result<String> {
                result.attrs_only(&[])?;
                ensure!(result.children.is_empty(), "result expression must be text");
                Ok(result.text.clone())
            }).transpose()?;
            let verification = match service.attr("verification").unwrap_or("human") {
                "human" => ActivityVerification::Human,
                "condition" => {
                    let condition = service
                        .child(TF, "condition")?
                        .context("condition verification requires expression")?;
                    condition.attrs_only(&[])?;
                    ensure!(
                        condition.children.is_empty(),
                        "verification condition must be text"
                    );
                    ActivityVerification::Condition {
                        expression: condition.text.clone(),
                    }
                }
                other => bail!(
                    "unsupported verification {other} at byte {}",
                    service.offset
                ),
            };
            if matches!(verification, ActivityVerification::Human) {
                ensure!(
                    service.child(TF, "condition")?.is_none(),
                    "human verification cannot have condition"
                );
            }
            ProcessNodeKind::ServiceTask {
                flow_id: service.required("flowId")?,
                input_mapping: mapping(service, "inputMapping")?,
                output_mapping: mapping(service, "outputMapping")?,
                verification,
                timeout_seconds: service
                    .required("timeoutSeconds")?
                    .parse()
                    .context("invalid service timeout")?,
                result_expression,
            }
        }
        "subProcess" => {
            element.attrs_only(&["id", "name", "triggeredByEvent"])?;
            ensure!(element.attr("triggeredByEvent").is_none_or(|value| value == "false"),
                "event subprocess {} is unsupported at byte {}", id, element.offset);
            element.children_only(&GRAPH_ELEMENTS)?;
            let extension = element.child(BPMN, "extensionElements")?
                .context("embedded subprocess requires TentaFlow extension")?;
            extension.attrs_only(&[])?;
            extension.children_only(&[(TF, "subProcess")])?;
            ensure!(extension.children.len() == 1, "embedded subprocess requires one TentaFlow extension");
            let config = extension.child(TF, "subProcess")?.expect("validated subprocess extension");
            config.attrs_only(&[])?;
            config.children_only(&[(TF, "variables"), (TF, "inputMapping"), (TF, "outputMapping")])?;
            let variables = config.child(TF, "variables")?
                .context("subprocess requires local variables")?;
            variables.attrs_only(&[])?;
            ensure!(variables.children.is_empty() && variables.text.len() <= MAX_VARIABLE_BYTES,
                "invalid subprocess variables at byte {}", variables.offset);
            let variables: BTreeMap<String, serde_json::Value> =
                serde_json::from_str(variables.text.trim()).map_err(|error| XmlElementError {
                    message: format!("invalid subprocess variables at byte {}: {error}", variables.offset),
                    element_id: Some(id.clone()), offset: variables.offset,
                })?;
            validate_variables(&serde_json::to_value(&variables)?)?;
            let (nodes, sequence_flows) = graph_from_xml(element, target_namespace)?;
            ProcessNodeKind::SubProcess {
                body: ProcessSubProcess { nodes, sequence_flows, variables, diagram: ProcessDiagram::default() },
                input_mapping: mapping(config, "inputMapping")?,
                output_mapping: mapping(config, "outputMapping")?,
            }
        }
        "callActivity" => {
            element.attrs_only(&["id", "name", "calledElement"])?;
            element.children_only(&[(BPMN, "extensionElements")])?;
            ensure!(element.text.trim().is_empty(),
                "call activity {} has unsupported text at byte {}", id, element.offset);
            let (namespace_uri, process_id) = element.qnames.get("calledElement")
                .ok_or_else(|| XmlElementError {
                    message: format!("call activity requires a bound calledElement QName at byte {}", element.offset),
                    element_id: Some(id.clone()), offset: element.offset,
                })?;
            ensure!(!namespace_uri.is_empty(),
                "call activity {} has unbound calledElement at byte {}", id, element.offset);
            let extension = element.child(BPMN, "extensionElements")?
                .ok_or_else(|| XmlElementError {
                    message: format!("call activity requires a private TentaFlow target binding at byte {}", element.offset),
                    element_id: Some(id.clone()), offset: element.offset,
                })?;
            extension.attrs_only(&[])?;
            extension.children_only(&[(TF, "callActivity")])?;
            ensure!(extension.children.len() == 1,
                "call activity {} requires exactly one private target binding", id);
            let binding = extension.child(TF, "callActivity")?.expect("validated call binding");
            binding.attrs_only(&["definitionId", "version"])?;
            binding.children_only(&[(TF, "inputMapping"), (TF, "outputMapping")])?;
            ensure!(binding.text.trim().is_empty(),
                "call activity {} has unsupported binding text at byte {}", id, binding.offset);
            let target_attribute = |name: &str| binding.attr(name).ok_or_else(|| XmlElementError {
                message: format!("call activity requires {name} at byte {}", binding.offset),
                element_id: Some(id.clone()), offset: binding.offset,
            });
            let called_definition_id = target_attribute("definitionId")?.to_string();
            let called_version = target_attribute("version")?.parse().map_err(|error| XmlElementError {
                message: format!("invalid call version at byte {}: {error}", binding.offset),
                element_id: Some(id.clone()), offset: binding.offset,
            })?;
            ProcessNodeKind::CallActivity {
                called_definition_id,
                called_version,
                called_element: ProcessCallableReference {
                    namespace_uri: namespace_uri.clone(), process_id: process_id.clone(),
                },
                input_mapping: mapping(binding, "inputMapping")?,
                output_mapping: mapping(binding, "outputMapping")?,
            }
        }
        other => bail!("unsupported BPMN node {other} at byte {}", element.offset),
    };
    match &kind {
        ProcessNodeKind::TimerStart { timer }
        | ProcessNodeKind::TimerCatch { timer }
        | ProcessNodeKind::BoundaryTimer { timer, .. } => {
            validate_timer_spec(timer, matches!(&kind, ProcessNodeKind::TimerStart { .. }))
                .map_err(|error| XmlElementError {
                    message: format!("invalid timer rule at byte {}: {error}", element.offset),
                    element_id: Some(id.clone()), offset: element.offset,
                })?;
        }
        _ => {}
    }
    Ok(ProcessNode { id, name, kind })
}

fn diagram_from_xml(element: &Element, process_id: &str) -> Result<ProcessDiagram> {
    element.attrs_only(&["id"])?;
    element.children_only(&[(BPMNDI, "BPMNPlane")])?;
    ensure!(
        element.children.len() == 1,
        "BPMNDiagram requires one plane"
    );
    let plane = element
        .child(BPMNDI, "BPMNPlane")?
        .expect("validated plane");
    plane.attrs_only(&["id", "bpmnElement"])?;
    ensure!(
        plane.attr("bpmnElement") == Some(process_id),
        "BPMNPlane references a different process"
    );
    plane.children_only(&[(BPMNDI, "BPMNShape"), (BPMNDI, "BPMNEdge")])?;
    let mut diagram = ProcessDiagram::default();
    for child in &plane.children {
        if child.is(BPMNDI, "BPMNShape") {
            child.attrs_only(&["id", "bpmnElement", "isExpanded"])?;
            if let Some(expanded) = child.attr("isExpanded") {
                if expanded != "false" && expanded != "0" {
                    return Err(XmlElementError {
                        message: format!("expanded subprocess diagram is unsupported at byte {}", child.offset),
                        element_id: Some(child.required("bpmnElement")?), offset: child.offset,
                    }.into());
                }
            }
            child.children_only(&[(DC, "Bounds")])?;
            ensure!(child.children.len() == 1, "BPMNShape requires Bounds");
            let bounds = child.child(DC, "Bounds")?.expect("validated Bounds");
            bounds.attrs_only(&["x", "y", "width", "height"])?;
            ensure!(bounds.children.is_empty(), "Bounds cannot contain elements");
            diagram.shapes.push(ProcessShape {
                element_id: child.required("bpmnElement")?,
                x: bounds.required("x")?.parse()?,
                y: bounds.required("y")?.parse()?,
                width: bounds.required("width")?.parse()?,
                height: bounds.required("height")?.parse()?,
            });
        } else {
            child.attrs_only(&["id", "bpmnElement"])?;
            child.children_only(&[(DI, "waypoint")])?;
            let mut waypoints = Vec::new();
            for point in &child.children {
                point.attrs_only(&["x", "y"])?;
                ensure!(
                    point.children.is_empty(),
                    "waypoint cannot contain elements"
                );
                waypoints.push(ProcessPoint {
                    x: point.required("x")?.parse()?,
                    y: point.required("y")?.parse()?,
                });
            }
            diagram.edges.push(ProcessEdgeDiagram {
                sequence_flow_id: child.required("bpmnElement")?,
                waypoints,
            });
        }
    }
    Ok(diagram)
}

fn partition_body_diagram(body: &mut ProcessSubProcess, diagram: &mut ProcessDiagram) {
    for node in &mut body.nodes {
        if let ProcessNodeKind::SubProcess { body: child, .. } = &mut node.kind {
            partition_body_diagram(child, diagram);
        }
    }
    let node_ids: HashSet<&str> = body.nodes.iter().map(|node| node.id.as_str()).collect();
    let flow_ids: HashSet<&str> = body.sequence_flows.iter().map(|flow| flow.id.as_str()).collect();
    let (owned_shapes, remaining_shapes) = std::mem::take(&mut diagram.shapes)
        .into_iter().partition(|shape| node_ids.contains(shape.element_id.as_str()));
    let (owned_edges, remaining_edges) = std::mem::take(&mut diagram.edges)
        .into_iter().partition(|edge| flow_ids.contains(edge.sequence_flow_id.as_str()));
    body.diagram = ProcessDiagram { shapes: owned_shapes, edges: owned_edges };
    diagram.shapes = remaining_shapes;
    diagram.edges = remaining_edges;
}

fn graph_from_xml(element: &Element, namespace: &str) -> Result<(Vec<ProcessNode>, Vec<ProcessSequenceFlow>)> {
    let mut nodes = Vec::new();
    let mut sequence_flows = Vec::new();
    for child in &element.children {
        if child.is(BPMN, "extensionElements") { continue; }
        if child.is(BPMN, "sequenceFlow") {
            child.attrs_only(&["id", "sourceRef", "targetRef"])?;
            child.children_only(&[(BPMN, "conditionExpression")])?;
            ensure!(child.children.len() <= 1, "sequence flow has multiple conditions");
            let condition = child.child(BPMN, "conditionExpression")?
                .map(|condition| {
                    condition.attrs_only(&["language"])?;
                    if let Some(language) = condition.attr("language") {
                        ensure!(language == "https://cel.dev/spec", "unsupported expression language");
                    }
                    ensure!(condition.children.is_empty(), "condition expression must be text");
                    Ok(condition.text.clone())
                }).transpose()?;
            sequence_flows.push(ProcessSequenceFlow {
                id: child.required("id")?, source_id: child.required("sourceRef")?,
                target_id: child.required("targetRef")?, condition,
            });
        } else {
            nodes.push(node_from_xml(child, namespace).map_err(|error| {
                if error.downcast_ref::<XmlElementError>().is_some() { error }
                else { XmlElementError {
                    message: format!("invalid BPMN element {} at byte {}: {error}", child.local, child.offset),
                    element_id: child.attr("id").map(str::to_string), offset: child.offset,
                }.into() }
            })?);
        }
    }
    Ok((nodes, sequence_flows))
}

fn parse_model(xml: &str) -> Result<ProcessModel> {
    let root = parse_tree(xml)?;
    ensure!(
        root.is(BPMN, "definitions"),
        "BPMN root must use the BPMN model namespace"
    );
    root.attrs_only(&["id", "targetNamespace"])?;
    root.children_only(&[(BPMN, "process"), (BPMNDI, "BPMNDiagram"), (BPMN, "message"), (BPMN, "error")])?;
    let has_declarations = root.children.iter().any(|child| child.is(BPMN, "message") || child.is(BPMN, "error"));
    if has_declarations {
        ensure!(root.attr("targetNamespace").is_some_and(|value| !value.is_empty()),
            "BPMN declarations require explicit targetNamespace");
    }
    let namespace = root.attr("targetNamespace").unwrap_or(TF);
    let mut messages = Vec::new();
    let mut errors = Vec::new();
    for child in &root.children {
        if child.is(BPMN, "message") {
            child.attrs_only(&["id", "name"])?;
            ensure!(child.children.is_empty() && child.text.trim().is_empty(), "message declaration must be empty");
            messages.push(ProcessMessageDeclaration { message_id: child.required("id")?, name: child.required("name")? });
        } else if child.is(BPMN, "error") {
            child.attrs_only(&["id", "name", "errorCode"])?;
            ensure!(child.children.is_empty() && child.text.trim().is_empty(), "error declaration must be empty");
            errors.push(ProcessErrorDeclaration { error_id: child.required("id")?, name: child.required("name")?, error_code: child.required("errorCode")? });
        }
    }
    let processes: Vec<_> = root
        .children
        .iter()
        .filter(|child| child.is(BPMN, "process"))
        .collect();
    ensure!(processes.len() == 1, "B1 XML requires exactly one process");
    let process = processes[0];
    process.attrs_only(&["id", "name", "isExecutable"])?;
    ensure!(
        process
            .attr("isExecutable")
            .is_none_or(|value| value == "true"),
        "non-executable process is unsupported"
    );
    process.children_only(&GRAPH_ELEMENTS)?;
    let process_id = process.required("id")?;
    let (variables, timer_timezone, work_calendar, calendar_pin) = process_configuration(process, &process_id)?;
    let (mut nodes, sequence_flows) = graph_from_xml(process, namespace)?;
    for node in &nodes {
        let message_ref = match &node.kind {
            ProcessNodeKind::MessageStart { message_ref, .. }
            | ProcessNodeKind::MessageCatch { message_ref, .. }
            | ProcessNodeKind::MessageThrow { message_ref, .. }
            | ProcessNodeKind::BoundaryMessage { message_ref, .. } => Some(message_ref.as_str()),
            _ => None,
        };
        let error_ref = match &node.kind {
            ProcessNodeKind::BoundaryError { error_ref, .. } => error_ref.as_deref(),
            ProcessNodeKind::ErrorEnd { error_ref } => Some(error_ref.as_str()),
            _ => None,
        };
        if message_ref.is_some_and(|reference| !messages.iter().any(|declaration| declaration.message_id == reference))
            || error_ref.is_some_and(|reference| !errors.iter().any(|declaration| declaration.error_id == reference)) {
            let offset = process.children.iter().find(|child| child.attr("id") == Some(node.id.as_str()))
                .map_or(process.offset, |child| child.offset);
            return Err(XmlElementError {
                message: format!("event {} references an unknown or wrong-type declaration at byte {offset}", node.id),
                element_id: Some(node.id.clone()), offset,
            }.into());
        }
    }
    if timer_timezone.is_none() {
        if let Some(node) = nodes.iter().find(|node| matches!(&node.kind,
            ProcessNodeKind::TimerStart { .. }
            | ProcessNodeKind::TimerCatch { .. }
            | ProcessNodeKind::BoundaryTimer { .. })) {
            let offset = process.children.iter().find(|child| child.attr("id") == Some(node.id.as_str()))
                .map_or(process.offset, |child| child.offset);
            return Err(XmlElementError {
                message: format!("timed process requires timerTimezone at byte {offset}"),
                element_id: Some(node.id.clone()), offset,
            }.into());
        }
    }
    let diagrams: Vec<_> = root
        .children
        .iter()
        .filter(|child| child.is(BPMNDI, "BPMNDiagram"))
        .collect();
    ensure!(diagrams.len() <= 1, "B1 XML supports one BPMN diagram");
    let mut diagram = diagrams
        .first()
        .map(|element| diagram_from_xml(element, &process_id))
        .transpose()?
        .unwrap_or_default();
    for node in &mut nodes {
        if let ProcessNodeKind::SubProcess { body, .. } = &mut node.kind {
            partition_body_diagram(body, &mut diagram);
        }
    }
    let model = ProcessModel {
        schema_version: 1,
        process_id,
        nodes,
        sequence_flows,
        variables,
        diagram,
        timer_timezone,
        work_calendar,
        calendar_pin,
        messages,
        errors,
        target_namespace: (namespace != TF).then(|| namespace.to_string()),
    };
    validate_model(&model).map_err(|error| XmlElementError {
        message: error.to_string(),
        element_id: Some(model.process_id.clone()),
        offset: process.offset,
    })?;
    Ok(model)
}

pub fn import_xml(xml: &str) -> (Option<ProcessModel>, Vec<ProcessDiagnostic>) {
    match parse_model(xml) {
        Ok(model) => (Some(model), Vec::new()),
        Err(error) => {
            let element = error.downcast_ref::<XmlElementError>();
            (
                None,
                vec![ProcessDiagnostic {
                    code: "UNSUPPORTED_OR_INVALID_BPMN".into(),
                    message: error.to_string(),
                    element_id: element.and_then(|element| element.element_id.clone()),
                    offset: element.map(|element| element.offset),
                    fatal: true,
                }],
            )
        }
    }
}

fn escaped(text: &str) -> String {
    quick_xml::escape::escape(text).replace('\r', "&#13;")
}

fn generated_xml_id(preferred: &str, used: &mut HashSet<String>) -> String {
    let mut candidate = preferred.to_string();
    let mut suffix = 2;
    while !used.insert(candidate.clone()) {
        candidate = format!("{preferred}_{suffix}");
        suffix += 1;
    }
    candidate
}

fn collect_diagram<'a>(
    nodes: &'a [ProcessNode], diagram: &'a ProcessDiagram,
    shapes: &mut Vec<&'a ProcessShape>, edges: &mut Vec<&'a ProcessEdgeDiagram>,
    used_ids: &mut HashSet<String>,
) {
    used_ids.extend(nodes.iter().map(|node| node.id.clone()));
    shapes.extend(&diagram.shapes);
    edges.extend(&diagram.edges);
    for node in nodes {
        if let ProcessNodeKind::SubProcess { body, .. } = &node.kind {
            used_ids.extend(body.sequence_flows.iter().map(|flow| flow.id.clone()));
            collect_diagram(&body.nodes, &body.diagram, shapes, edges, used_ids);
        }
    }
}

fn write_graph(xml: &mut String, nodes: &[ProcessNode], flows: &[ProcessSequenceFlow],
    call_prefixes: &BTreeMap<String, String>) -> Result<()> {
    for node in nodes {
        let (tag, extra) = match &node.kind {
            ProcessNodeKind::Start => ("startEvent", String::new()),
            ProcessNodeKind::TimerStart { .. } => ("startEvent", String::new()),
            ProcessNodeKind::MessageStart { .. } => ("startEvent", String::new()),
            ProcessNodeKind::TimerCatch { .. } => ("intermediateCatchEvent", String::new()),
            ProcessNodeKind::MessageCatch { .. } => ("intermediateCatchEvent", String::new()),
            ProcessNodeKind::MessageThrow { .. } => ("intermediateThrowEvent", String::new()),
            ProcessNodeKind::BoundaryTimer { attached_to_id, cancel_activity, .. } => (
                "boundaryEvent",
                format!(" attachedToRef=\"{}\" cancelActivity=\"{}\"",
                    escaped(attached_to_id), cancel_activity),
            ),
            ProcessNodeKind::BoundaryMessage { attached_to_id, cancel_activity, .. } => (
                "boundaryEvent",
                format!(" attachedToRef=\"{}\" cancelActivity=\"{}\"", escaped(attached_to_id), cancel_activity),
            ),
            ProcessNodeKind::BoundaryError { attached_to_id, .. } => (
                "boundaryEvent",
                format!(" attachedToRef=\"{}\" cancelActivity=\"true\"", escaped(attached_to_id)),
            ),
            ProcessNodeKind::End => ("endEvent", String::new()),
            ProcessNodeKind::ErrorEnd { .. } => ("endEvent", String::new()),
            ProcessNodeKind::ParallelGateway => ("parallelGateway", String::new()),
            ProcessNodeKind::EventBasedGateway => (
                "eventBasedGateway", " gatewayDirection=\"Diverging\" eventGatewayType=\"Exclusive\" instantiate=\"false\"".into()),
            ProcessNodeKind::ExclusiveGateway { default_flow_id } => (
                "exclusiveGateway",
                default_flow_id
                    .as_ref()
                    .map(|id| format!(" default=\"{}\"", escaped(id)))
                    .unwrap_or_default(),
            ),
            ProcessNodeKind::UserTask { .. } => ("userTask", String::new()),
            ProcessNodeKind::ServiceTask { .. } => ("serviceTask", String::new()),
            ProcessNodeKind::SubProcess { .. } => ("subProcess", String::new()),
            ProcessNodeKind::CallActivity { called_element, .. } => {
                let prefix = call_prefixes.get(&called_element.namespace_uri)
                    .context("validated call namespace lacks XML prefix")?;
                ("callActivity", format!(" calledElement=\"{prefix}:{}\"",
                    escaped(&called_element.process_id)))
            }
        };
        xml.push_str(&format!(
            "<bpmn:{tag} id=\"{}\" name=\"{}\"{extra}",
            escaped(&node.id),
            escaped(&node.name)
        ));
        match &node.kind {
            ProcessNodeKind::TimerStart { timer }
            | ProcessNodeKind::TimerCatch { timer }
            | ProcessNodeKind::BoundaryTimer { timer, .. } => {
                let rule = match timer {
                    ProcessTimerSpec::Date { at } => format!("<bpmn:timeDate>{}</bpmn:timeDate>", escaped(at)),
                    ProcessTimerSpec::Duration { seconds } => format!("<bpmn:timeDuration>PT{seconds}S</bpmn:timeDuration>"),
                    ProcessTimerSpec::Cycle { seconds, total_firings } => format!(
                        "<bpmn:timeCycle>R{}/PT{seconds}S</bpmn:timeCycle>",
                        total_firings.map(|count| count.to_string()).unwrap_or_default()
                    ),
                    ProcessTimerSpec::Daily { hour, minute, total_firings } => format!(
                        "<bpmn:extensionElements><tentaflow:dailyTimer hour=\"{hour}\" minute=\"{minute}\"{} /></bpmn:extensionElements>",
                        total_firings.map(|count| format!(" totalFirings=\"{count}\"")).unwrap_or_default()
                    ),
                    ProcessTimerSpec::WorkingDuration { seconds } => format!(
                        "<bpmn:extensionElements><tentaflow:workingDuration seconds=\"{seconds}\" /></bpmn:extensionElements>"
                    ),
                };
                xml.push_str(&format!("><bpmn:timerEventDefinition>{rule}</bpmn:timerEventDefinition></bpmn:{tag}>"));
            }
            ProcessNodeKind::MessageStart { message_ref, output_mapping } => {
                let config = serde_json::json!({ "output_mapping": output_mapping });
                xml.push_str(&format!("><bpmn:messageEventDefinition messageRef=\"tns:{}\"/><bpmn:extensionElements><tentaflow:message>{}</tentaflow:message></bpmn:extensionElements></bpmn:{tag}>",
                    escaped(message_ref), escaped(&serde_json::to_string(&config)?)));
            }
            ProcessNodeKind::MessageCatch { message_ref, correlation_expression, output_mapping }
            | ProcessNodeKind::BoundaryMessage { message_ref, correlation_expression, output_mapping, .. } => {
                let config = serde_json::json!({ "correlation_expression": correlation_expression, "output_mapping": output_mapping });
                xml.push_str(&format!("><bpmn:messageEventDefinition messageRef=\"tns:{}\"/><bpmn:extensionElements><tentaflow:message>{}</tentaflow:message></bpmn:extensionElements></bpmn:{tag}>",
                    escaped(message_ref), escaped(&serde_json::to_string(&config)?)));
            }
            ProcessNodeKind::MessageThrow { message_ref, target, correlation_expression, payload_expression, ttl_seconds } => {
                let config = serde_json::json!({ "target": target, "correlation_expression": correlation_expression,
                    "payload_expression": payload_expression, "ttl_seconds": ttl_seconds });
                xml.push_str(&format!("><bpmn:messageEventDefinition messageRef=\"tns:{}\"/><bpmn:extensionElements><tentaflow:message>{}</tentaflow:message></bpmn:extensionElements></bpmn:{tag}>",
                    escaped(message_ref), escaped(&serde_json::to_string(&config)?)));
            }
            ProcessNodeKind::BoundaryError { error_ref, output_mapping, .. } => {
                let reference = error_ref.as_ref().map(|id| format!(" errorRef=\"tns:{}\"", escaped(id))).unwrap_or_default();
                xml.push_str(&format!("><bpmn:errorEventDefinition{reference}/>"));
                if !output_mapping.is_empty() {
                    xml.push_str(&format!("<bpmn:extensionElements><tentaflow:outputMapping>{}</tentaflow:outputMapping></bpmn:extensionElements>",
                        escaped(&serde_json::to_string(output_mapping)?)));
                }
                xml.push_str(&format!("</bpmn:{tag}>"));
            }
            ProcessNodeKind::ErrorEnd { error_ref } => {
                xml.push_str(&format!("><bpmn:errorEventDefinition errorRef=\"tns:{}\"/></bpmn:{tag}>",
                    escaped(error_ref)));
            }
            ProcessNodeKind::UserTask {
                assignee_user_id,
                output_mapping,
            } => {
                if assignee_user_id.is_none() && output_mapping.is_empty() {
                    xml.push_str("/>");
                } else {
                    xml.push_str("><bpmn:extensionElements><tentaflow:user");
                    if let Some(id) = assignee_user_id {
                        xml.push_str(&format!(" assigneeUserId=\"{}\"", escaped(id)));
                    }
                    xml.push('>');
                    if !output_mapping.is_empty() {
                        xml.push_str(&format!(
                            "<tentaflow:outputMapping>{}</tentaflow:outputMapping>",
                            escaped(&serde_json::to_string(output_mapping)?)
                        ));
                    }
                    xml.push_str("</tentaflow:user></bpmn:extensionElements>");
                    xml.push_str(&format!("</bpmn:{tag}>"));
                }
            }
            ProcessNodeKind::ServiceTask {
                flow_id,
                input_mapping,
                output_mapping,
                verification,
                timeout_seconds,
                result_expression,
            } => {
                let verify = if matches!(verification, ActivityVerification::Human) {
                    "human"
                } else {
                    "condition"
                };
                xml.push_str(&format!("><bpmn:extensionElements><tentaflow:service flowId=\"{}\" timeoutSeconds=\"{timeout_seconds}\" verification=\"{verify}\">",escaped(flow_id)));
                if !input_mapping.is_empty() {
                    xml.push_str(&format!(
                        "<tentaflow:inputMapping>{}</tentaflow:inputMapping>",
                        escaped(&serde_json::to_string(input_mapping)?)
                    ));
                }
                if !output_mapping.is_empty() {
                    xml.push_str(&format!(
                        "<tentaflow:outputMapping>{}</tentaflow:outputMapping>",
                        escaped(&serde_json::to_string(output_mapping)?)
                    ));
                }
                if let ActivityVerification::Condition { expression } = verification {
                    xml.push_str(&format!(
                        "<tentaflow:condition>{}</tentaflow:condition>",
                        escaped(expression)
                    ));
                }
                if let Some(expression) = result_expression {
                    xml.push_str(&format!("<tentaflow:resultExpression>{}</tentaflow:resultExpression>", escaped(expression)));
                }
                xml.push_str("</tentaflow:service></bpmn:extensionElements>");
                xml.push_str(&format!("</bpmn:{tag}>"));
            }
            ProcessNodeKind::SubProcess { body, input_mapping, output_mapping } => {
                xml.push_str("><bpmn:extensionElements><tentaflow:subProcess>");
                xml.push_str(&format!("<tentaflow:variables>{}</tentaflow:variables>",
                    escaped(&serde_json::to_string(&body.variables)?)));
                if !input_mapping.is_empty() {
                    xml.push_str(&format!("<tentaflow:inputMapping>{}</tentaflow:inputMapping>",
                        escaped(&serde_json::to_string(input_mapping)?)));
                }
                if !output_mapping.is_empty() {
                    xml.push_str(&format!("<tentaflow:outputMapping>{}</tentaflow:outputMapping>",
                        escaped(&serde_json::to_string(output_mapping)?)));
                }
                xml.push_str("</tentaflow:subProcess></bpmn:extensionElements>");
                write_graph(xml, &body.nodes, &body.sequence_flows, call_prefixes)?;
                xml.push_str(&format!("</bpmn:{tag}>"));
            }
            ProcessNodeKind::CallActivity { called_definition_id, called_version,
                input_mapping, output_mapping, .. } => {
                xml.push_str(&format!("><bpmn:extensionElements><tentaflow:callActivity definitionId=\"{}\" version=\"{}\">",
                    escaped(called_definition_id), called_version));
                if !input_mapping.is_empty() {
                    xml.push_str(&format!("<tentaflow:inputMapping>{}</tentaflow:inputMapping>",
                        escaped(&serde_json::to_string(input_mapping)?)));
                }
                if !output_mapping.is_empty() {
                    xml.push_str(&format!("<tentaflow:outputMapping>{}</tentaflow:outputMapping>",
                        escaped(&serde_json::to_string(output_mapping)?)));
                }
                xml.push_str(&format!("</tentaflow:callActivity></bpmn:extensionElements></bpmn:{tag}>"));
            }
            _ => xml.push_str("/>"),
        }
    }
    for flow in flows {
        xml.push_str(&format!(
            "<bpmn:sequenceFlow id=\"{}\" sourceRef=\"{}\" targetRef=\"{}\"",
            escaped(&flow.id),
            escaped(&flow.source_id),
            escaped(&flow.target_id)
        ));
        if let Some(expression) = &flow.condition {
            xml.push_str(&format!("><bpmn:conditionExpression language=\"https://cel.dev/spec\">{}</bpmn:conditionExpression></bpmn:sequenceFlow>",escaped(expression)));
        } else {
            xml.push_str("/>");
        }
    }
    Ok(())
}

pub fn export_xml(model: &ProcessModel) -> Result<String> {
    validate_model(model)?;
    let namespace = model.target_namespace.as_deref().unwrap_or(TF);
    let declarations = !model.messages.is_empty() || !model.errors.is_empty();
    let call_namespaces: BTreeSet<_> = super::model::all_nodes(model).into_iter().filter_map(|node| {
        if let ProcessNodeKind::CallActivity { called_element, .. } = &node.kind {
            Some(called_element.namespace_uri.clone())
        } else { None }
    }).collect();
    let mut call_prefixes = BTreeMap::new();
    let tns = if declarations || call_namespaces.contains(namespace) {
        call_prefixes.insert(namespace.to_string(), "tns".to_string());
        format!(" xmlns:tns=\"{}\"", escaped(namespace))
    } else { String::new() };
    let mut call_namespaces_xml = String::new();
    for (index, uri) in call_namespaces.into_iter().filter(|uri| uri.as_str() != namespace).enumerate() {
        let prefix = format!("call{}", index + 1);
        call_namespaces_xml.push_str(&format!(" xmlns:{prefix}=\"{}\"", escaped(&uri)));
        call_prefixes.insert(uri, prefix);
    }
    let mut xml=format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><bpmn:definitions xmlns:bpmn=\"{BPMN}\" xmlns:bpmndi=\"{BPMNDI}\" xmlns:dc=\"{DC}\" xmlns:di=\"{DI}\" xmlns:tentaflow=\"{TF}\"{tns}{call_namespaces_xml} targetNamespace=\"{}\">", escaped(namespace));
    for message in &model.messages {
        xml.push_str(&format!("<bpmn:message id=\"{}\" name=\"{}\"/>", escaped(&message.message_id), escaped(&message.name)));
    }
    for error in &model.errors {
        xml.push_str(&format!("<bpmn:error id=\"{}\" name=\"{}\" errorCode=\"{}\"/>", escaped(&error.error_id), escaped(&error.name), escaped(&error.error_code)));
    }
    xml.push_str(&format!("<bpmn:process id=\"{}\" isExecutable=\"true\">", escaped(&model.process_id)));
    xml.push_str(&format!(
        "<bpmn:extensionElements><tentaflow:variables>{}</tentaflow:variables>",
        escaped(&serde_json::to_string(&model.variables)?)
    ));
    if let Some(timezone) = &model.timer_timezone {
        xml.push_str(&format!("<tentaflow:timerTimezone>{}</tentaflow:timerTimezone>", escaped(timezone)));
    }
    if let Some(calendar) = &model.work_calendar {
        xml.push_str(&format!("<tentaflow:workCalendar>{}</tentaflow:workCalendar>", escaped(&serde_json::to_string(calendar)?)));
    }
    if let Some(pin) = &model.calendar_pin {
        xml.push_str(&format!("<tentaflow:calendarPin>{}</tentaflow:calendarPin>", escaped(&serde_json::to_string(pin)?)));
    }
    xml.push_str("</bpmn:extensionElements>");
    write_graph(&mut xml, &model.nodes, &model.sequence_flows, &call_prefixes)?;
    xml.push_str("</bpmn:process>");
    let mut used_ids = HashSet::from([model.process_id.clone()]);
    used_ids.extend(model.messages.iter().map(|declaration| declaration.message_id.clone()));
    used_ids.extend(model.errors.iter().map(|declaration| declaration.error_id.clone()));
    used_ids.extend(model.sequence_flows.iter().map(|flow| flow.id.clone()));
    let mut shapes = Vec::new();
    let mut edges = Vec::new();
    collect_diagram(&model.nodes, &model.diagram, &mut shapes, &mut edges, &mut used_ids);
    if !shapes.is_empty() || !edges.is_empty() {
        let subprocess_ids: HashSet<&str> = super::model::all_nodes(model).into_iter()
            .filter(|node| matches!(node.kind, ProcessNodeKind::SubProcess { .. } | ProcessNodeKind::CallActivity { .. }))
            .map(|node| node.id.as_str()).collect();
        let diagram_id = generated_xml_id("Diagram_1", &mut used_ids);
        let plane_id = generated_xml_id("Plane_1", &mut used_ids);
        xml.push_str(&format!("<bpmndi:BPMNDiagram id=\"{}\"><bpmndi:BPMNPlane id=\"{}\" bpmnElement=\"{}\">",escaped(&diagram_id),escaped(&plane_id),escaped(&model.process_id)));
        for shape in shapes {
            let shape_id = generated_xml_id(&format!("DI_{}", shape.element_id), &mut used_ids);
            let collapsed = if subprocess_ids.contains(shape.element_id.as_str()) { " isExpanded=\"false\"" } else { "" };
            xml.push_str(&format!("<bpmndi:BPMNShape id=\"{}\" bpmnElement=\"{}\"{collapsed}><dc:Bounds x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></bpmndi:BPMNShape>",escaped(&shape_id),escaped(&shape.element_id),shape.x,shape.y,shape.width,shape.height));
        }
        for edge in edges {
            let edge_id = generated_xml_id(&format!("DI_{}", edge.sequence_flow_id), &mut used_ids);
            xml.push_str(&format!(
                "<bpmndi:BPMNEdge id=\"{}\" bpmnElement=\"{}\">",
                escaped(&edge_id),
                escaped(&edge.sequence_flow_id)
            ));
            for point in &edge.waypoints {
                xml.push_str(&format!(
                    "<di:waypoint x=\"{}\" y=\"{}\"/>",
                    point.x, point.y
                ));
            }
            xml.push_str("</bpmndi:BPMNEdge>");
        }
        xml.push_str("</bpmndi:BPMNPlane></bpmndi:BPMNDiagram>");
    }
    xml.push_str("</bpmn:definitions>");
    ensure!(
        xml.len() <= MAX_MODEL_BYTES,
        "exported BPMN XML exceeds 512 KiB"
    );
    Ok(xml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_activity_and_error_end_xml_preserve_cross_namespace_binding_and_mappings() {
        let mut model = super::super::model::starter_model();
        model.target_namespace = Some("urn:orders:caller".into());
        model.errors.push(ProcessErrorDeclaration {
            error_id: "Error_Business".into(), name: "Rejected & returned".into(),
            error_code: "ORDER.REJECTED".into(),
        });
        model.nodes.insert(1, ProcessNode {
            id: "Call_1".into(), name: "Call & review".into(),
            kind: ProcessNodeKind::CallActivity {
                called_definition_id: "91764f75-dadb-41aa-a252-a8a911fe7a94".into(),
                called_version: 7,
                called_element: ProcessCallableReference {
                    namespace_uri: "urn:orders:callee".into(),
                    process_id: "Review_Process".into(),
                },
                input_mapping: BTreeMap::from([("customer_ID".into(), "vars.customer_ID".into())]),
                output_mapping: BTreeMap::from([("return_value".into(), "outputs.return_value".into())]),
            },
        });
        model.nodes[2].kind = ProcessNodeKind::ErrorEnd { error_ref: "Error_Business".into() };
        model.sequence_flows[0].target_id = "Call_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_2".into(), source_id: "Call_1".into(), target_id: "End_1".into(),
            condition: None,
        });
        model.diagram.shapes.push(ProcessShape {
            element_id: "Call_1".into(), x: 180.0, y: 120.0, width: 160.0, height: 100.0,
        });
        let xml = export_xml(&model).expect("valid private call XML");
        assert!(xml.contains("xmlns:call1=\"urn:orders:callee\""));
        assert!(xml.contains("calledElement=\"call1:Review_Process\""));
        assert!(xml.contains("isExpanded=\"false\""));
        assert!(xml.contains("errorRef=\"tns:Error_Business\""));
        let (restored, diagnostics) = import_xml(&xml);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(restored, Some(model));

        let unbound = xml.replace("<tentaflow:callActivity definitionId=\"91764f75-dadb-41aa-a252-a8a911fe7a94\" version=\"7\">",
            "<tentaflow:callActivity version=\"7\">");
        let (unsupported, diagnostics) = import_xml(&unbound);
        assert!(unsupported.is_none());
        assert_eq!(diagnostics[0].element_id.as_deref(), Some("Call_1"));
        assert!(diagnostics[0].offset.is_some());
        let wrong_qname = xml.replace("calledElement=\"call1:Review_Process\"",
            "calledElement=\"unknown:Review_Process\"");
        let (unsupported, diagnostics) = import_xml(&wrong_qname);
        assert!(unsupported.is_none());
        assert!(diagnostics[0].fatal);
    }

    #[test]
    fn message_and_error_xml_round_trip_preserves_custom_namespace_qnames_and_business_keys() {
        let mut model = super::super::model::starter_model();
        model.target_namespace = Some("urn:example:orders:v1".into());
        model.messages = vec![
            ProcessMessageDeclaration { message_id: "Message_Start".into(), name: "order.received".into() },
            ProcessMessageDeclaration { message_id: "Message_Throw".into(), name: "order.sent".into() },
        ];
        model.errors = vec![ProcessErrorDeclaration { error_id: "Error_Validation".into(), name: "Bad & <order>".into(), error_code: "BUSINESS.INVALID".into() }];
        model.nodes[0].kind = ProcessNodeKind::MessageStart { message_ref: "Message_Start".into(),
            output_mapping: BTreeMap::from([("customer_ID".into(), "outputs.customer_ID".into())]) };
        model.nodes.push(ProcessNode { id: "Service_1".into(), name: "Check".into(), kind: ProcessNodeKind::ServiceTask {
            flow_id: "flow-123".into(), input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
            verification: ActivityVerification::Human, timeout_seconds: 60,
            result_expression: Some("vars.business_result".into()),
        } });
        model.nodes.push(ProcessNode { id: "Throw_1".into(), name: "Send".into(), kind: ProcessNodeKind::MessageThrow {
            message_ref: "Message_Throw".into(), target: ProcessMessageTargetSpec::Catch {
                definition_id: "7c865aaa-febd-4621-9ae6-35977200a0fd".into(),
                instance_id_expression: Some("vars.instance_ID".into()), subscription_id_expression: None,
            }, correlation_expression: "vars.customer_ID".into(), payload_expression: "vars.payload".into(), ttl_seconds: 60,
        } });
        model.nodes.push(ProcessNode { id: "Boundary_Error".into(), name: "Error".into(), kind: ProcessNodeKind::BoundaryError {
            attached_to_id: "Service_1".into(), error_ref: Some("Error_Validation".into()), output_mapping: BTreeMap::new(),
        } });
        model.sequence_flows[0].target_id = "Service_1".into();
        for (id, source, target) in [
            ("Flow_2", "Service_1", "Throw_1"), ("Flow_3", "Throw_1", "End_1"),
            ("Flow_4", "Boundary_Error", "End_1"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow { id: id.into(), source_id: source.into(), target_id: target.into(), condition: None });
        }
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("targetNamespace=\"urn:example:orders:v1\""));
        assert!(xml.contains("xmlns:tns=\"urn:example:orders:v1\""));
        assert!(xml.contains("messageRef=\"tns:Message_Start\""));
        assert!(xml.contains("errorRef=\"tns:Error_Validation\""));
        assert!(xml.contains("<tentaflow:resultExpression>vars.business_result</tentaflow:resultExpression>"));
        let (restored, diagnostics) = import_xml(&xml);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(restored, Some(model.clone()));

        let equivalent_prefix = xml.replace("xmlns:tns=", "xmlns:orders=")
            .replace("tns:Message_", "orders:Message_").replace("tns:Error_", "orders:Error_");
        assert_eq!(import_xml(&equivalent_prefix).0, Some(model.clone()));
        for invalid in [
            xml.replace("tns:Message_Start", "other:Message_Start"),
            xml.replace("tns:Message_Start", "tns:Error_Validation"),
            xml.replace("tns:Message_Start", "tns:Missing"),
            xml.replace("<bpmn:message id=\"Message_Start\"", "<bpmn:message id=\"Flow_1\""),
        ] {
            let (parsed, diagnostics) = import_xml(&invalid);
            assert!(parsed.is_none(), "{invalid}");
            assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal && diagnostic.offset.is_some()), "{diagnostics:?}");
        }
    }

    #[test]
    fn generated_di_ids_avoid_valid_declaration_ids_without_changing_the_model() {
        for (declaration_id, generated_id) in [
            ("Diagram_1", "Diagram_1_2"),
            ("Plane_1", "Plane_1_2"),
            ("DI_Start_1", "DI_Start_1_2"),
            ("DI_Flow_1", "DI_Flow_1_2"),
        ] {
            let mut model = super::super::model::starter_model();
            model.target_namespace = Some("urn:example:orders:v1".into());
            model.messages = vec![ProcessMessageDeclaration {
                message_id: declaration_id.into(),
                name: "order.received".into(),
            }];
            model.nodes[0].kind = ProcessNodeKind::MessageStart {
                message_ref: declaration_id.into(),
                output_mapping: BTreeMap::new(),
            };
            model.diagram.shapes.push(ProcessShape {
                element_id: "Start_1".into(), x: 20.0, y: 30.0, width: 56.0, height: 56.0,
            });
            model.diagram.edges.push(ProcessEdgeDiagram {
                sequence_flow_id: "Flow_1".into(),
                waypoints: vec![ProcessPoint { x: 76.0, y: 58.0 }, ProcessPoint { x: 180.0, y: 58.0 }],
            });
            let xml = export_xml(&model).unwrap();
            assert!(xml.contains(&format!("<bpmn:message id=\"{declaration_id}\"")));
            assert!(xml.contains(&format!("id=\"{generated_id}\"")), "{xml}");
            let (restored, diagnostics) = import_xml(&xml);
            assert!(diagnostics.is_empty(), "{diagnostics:?}");
            assert_eq!(restored, Some(model));
        }

        let mut plain = super::super::model::starter_model();
        plain.diagram.shapes.push(ProcessShape {
            element_id: "Start_1".into(), x: 20.0, y: 30.0, width: 56.0, height: 56.0,
        });
        let xml = export_xml(&plain).unwrap();
        assert!(xml.contains("<bpmndi:BPMNDiagram id=\"Diagram_1\"><bpmndi:BPMNPlane id=\"Plane_1\""));
        assert!(xml.contains("<bpmndi:BPMNShape id=\"DI_Start_1\" bpmnElement=\"Start_1\""));
    }

    #[test]
    fn import_rejects_duplicate_bpmn_and_di_ids_with_element_and_byte_context() {
        let mut model = super::super::model::starter_model();
        model.target_namespace = Some("urn:example:orders:v1".into());
        model.messages = vec![ProcessMessageDeclaration {
            message_id: "Diagram_1".into(), name: "order.received".into(),
        }];
        model.nodes[0].kind = ProcessNodeKind::MessageStart {
            message_ref: "Diagram_1".into(), output_mapping: BTreeMap::new(),
        };
        model.diagram.shapes.push(ProcessShape {
            element_id: "Start_1".into(), x: 20.0, y: 30.0, width: 56.0, height: 56.0,
        });
        let xml = export_xml(&model).unwrap();
        for (duplicate, id) in [
            (xml.replace("<bpmndi:BPMNDiagram id=\"Diagram_1_2\"", "<bpmndi:BPMNDiagram id=\"Diagram_1\""), "Diagram_1"),
            (xml.replace("<bpmndi:BPMNShape id=\"DI_Start_1\"", "<bpmndi:BPMNShape id=\"Start_1\""), "Start_1"),
        ] {
            let (parsed, diagnostics) = import_xml(&duplicate);
            assert!(parsed.is_none());
            assert_eq!(diagnostics.len(), 1);
            assert!(diagnostics[0].fatal);
            assert_eq!(diagnostics[0].element_id.as_deref(), Some(id));
            assert!(diagnostics[0].offset.is_some());
            assert!(diagnostics[0].message.contains("duplicate XML ID"));
        }
    }

    #[test]
    fn message_race_and_boundary_xml_round_trip_rejects_duplicate_extensions_with_context() {
        let mut model = super::super::model::starter_model();
        model.messages = vec![ProcessMessageDeclaration { message_id: "Message_1".into(), name: "order.received".into() }];
        model.nodes.extend([
            ProcessNode { id: "Race_1".into(), name: "First arrival".into(), kind: ProcessNodeKind::EventBasedGateway },
            ProcessNode { id: "Catch_A".into(), name: "First".into(), kind: ProcessNodeKind::MessageCatch {
                message_ref: "Message_1".into(), correlation_expression: "vars.case_ID".into(), output_mapping: BTreeMap::new(),
            } },
            ProcessNode { id: "Catch_B".into(), name: "Second".into(), kind: ProcessNodeKind::MessageCatch {
                message_ref: "Message_1".into(), correlation_expression: "vars.case_ID".into(), output_mapping: BTreeMap::new(),
            } },
            ProcessNode { id: "Service_1".into(), name: "Check".into(), kind: ProcessNodeKind::ServiceTask {
                flow_id: "flow-123".into(), input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
                verification: ActivityVerification::Human, timeout_seconds: 60, result_expression: None,
            } },
            ProcessNode { id: "Boundary_1".into(), name: "Reminder".into(), kind: ProcessNodeKind::BoundaryMessage {
                attached_to_id: "Service_1".into(), cancel_activity: false, message_ref: "Message_1".into(),
                correlation_expression: "vars.case_ID".into(), output_mapping: BTreeMap::new(),
            } },
        ]);
        model.sequence_flows[0].target_id = "Race_1".into();
        for (id, source, target) in [
            ("Flow_2", "Race_1", "Catch_A"), ("Flow_3", "Race_1", "Catch_B"),
            ("Flow_4", "Catch_A", "Service_1"), ("Flow_5", "Service_1", "End_1"),
            ("Flow_6", "Catch_B", "End_1"), ("Flow_7", "Boundary_1", "End_1"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow { id: id.into(), source_id: source.into(), target_id: target.into(), condition: None });
        }
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<bpmn:eventBasedGateway id=\"Race_1\""));
        assert!(xml.contains("cancelActivity=\"false\""));
        assert_eq!(import_xml(&xml).0, Some(model));

        let duplicate = xml.replacen("</tentaflow:message>",
            "</tentaflow:message><tentaflow:message>{}</tentaflow:message>", 1);
        let (parsed, diagnostics) = import_xml(&duplicate);
        assert!(parsed.is_none());
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].fatal && diagnostics[0].offset.is_some());
        assert_eq!(diagnostics[0].element_id.as_deref(), Some("Catch_A"));
    }

    #[test]
    fn working_calendar_xml_round_trips_without_changing_opaque_variables() {
        use tentaflow_protocol::processes::{HolidayPolicy, WorkWindow};
        let mut model = super::super::model::starter_model();
        model.timer_timezone = Some("Europe/Warsaw".into());
        model.work_calendar = Some(ProcessWorkCalendar {
            name: "R&D Łódź".into(),
            weekly_windows: vec![WorkWindow { weekday: 1, start_minute: 540, end_minute: 1020 }],
            manual_days_off: Vec::new(),
            holiday_policy: HolidayPolicy::None,
        });
        model.variables.insert("business_key".into(), serde_json::json!({"inner_value":"& <Łódź>"}));
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::WorkingDuration { seconds: 3600 },
        };
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<tentaflow:workingDuration seconds=\"3600\" />"));
        let (restored, diagnostics) = import_xml(&xml);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(restored.unwrap(), model);

        let duplicated = xml.replacen("</bpmn:extensionElements>",
            "<tentaflow:workCalendar>{}</tentaflow:workCalendar></bpmn:extensionElements>", 1);
        let (model, diagnostics) = import_xml(&duplicated);
        assert!(model.is_none());
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal));

        let malformed = xml.replacen("<tentaflow:workingDuration seconds=\"3600\" />",
            "<tentaflow:workingDuration seconds=\"3600\" fallback=\"elapsed\" />", 1);
        let (model, diagnostics) = import_xml(&malformed);
        assert!(model.is_none());
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal));
    }

    #[test]
    fn trusted_calendar_pin_xml_round_trips_stale_and_rejects_forged_digest() {
        use tentaflow_protocol::processes::{HolidayPolicy, WorkWindow};
        let mut model = super::super::model::starter_model();
        model.timer_timezone = Some("Europe/Warsaw".into());
        model.work_calendar = Some(ProcessWorkCalendar {
            name: "Office & contracts".into(),
            weekly_windows: vec![WorkWindow { weekday: 1, start_minute: 540, end_minute: 1020 }],
            manual_days_off: Vec::new(),
            holiday_policy: HolidayPolicy::PolandStatutory,
        });
        model.calendar_pin = Some(super::super::calendar::mint_calendar_pin(
            model.work_calendar.as_ref().unwrap(), "Europe/Warsaw",
        ).unwrap());
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<tentaflow:calendarPin>"));
        let (restored, diagnostics) = import_xml(&xml);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(restored.unwrap(), model);

        let pin = model.calendar_pin.as_mut().unwrap();
        pin.sha256 = "0".repeat(64);
        let forged = serde_json::to_string(pin).unwrap();
        let canonical = xml.split("<tentaflow:calendarPin>").nth(1).unwrap()
            .split("</tentaflow:calendarPin>").next().unwrap();
        let forged_xml = xml.replacen(canonical, &escaped(&forged), 1);
        let (rejected, diagnostics) = import_xml(&forged_xml);
        assert!(rejected.is_none());
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal && diagnostic.element_id.as_deref() == Some(model.process_id.as_str())));

        model.calendar_pin = Some(super::super::calendar::mint_calendar_pin(
            model.work_calendar.as_ref().unwrap(), "Europe/Warsaw",
        ).unwrap());
        model.work_calendar.as_mut().unwrap().name = "Revised office".into();
        let stale_xml = export_xml(&model).unwrap();
        let (restored, diagnostics) = import_xml(&stale_xml);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(restored.unwrap(), model);
    }

    #[test]
    fn boundary_xml_round_trip_preserves_siblings_attachment_cancellation_and_di() {
        let mut model = super::super::model::starter_model();
        model.timer_timezone = Some("Europe/Warsaw".into());
        model.variables.insert("business_key".into(), serde_json::json!({"label": "R&D Łódź"}));
        model.nodes.push(ProcessNode {
            id: "Review_1".into(), name: "Review & approve".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None, output_mapping: BTreeMap::new(),
            },
        });
        model.nodes.push(ProcessNode {
            id: "Boundary_A".into(), name: "Deadline".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Review_1".into(), cancel_activity: true,
                timer: ProcessTimerSpec::Date { at: "2027-01-02T03:04:05+01:00".into() },
            },
        });
        model.nodes.push(ProcessNode {
            id: "Boundary_B".into(), name: "Reminder".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Review_1".into(), cancel_activity: false,
                timer: ProcessTimerSpec::Duration { seconds: 90 },
            },
        });
        model.sequence_flows[0].target_id = "Review_1".into();
        for (id, source) in [
            ("Flow_2", "Review_1"), ("Flow_3", "Boundary_A"), ("Flow_4", "Boundary_B"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                id: id.into(), source_id: source.into(), target_id: "End_1".into(), condition: None,
            });
        }
        model.diagram.shapes.push(ProcessShape {
            element_id: "Boundary_A".into(), x: 125.0, y: 86.0, width: 36.0, height: 36.0,
        });
        model.diagram.shapes.push(ProcessShape {
            element_id: "Boundary_B".into(), x: 170.0, y: 86.0, width: 36.0, height: 36.0,
        });
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("attachedToRef=\"Review_1\" cancelActivity=\"true\""));
        assert!(xml.contains("attachedToRef=\"Review_1\" cancelActivity=\"false\""));
        let (parsed, diagnostics) = import_xml(&xml);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(parsed, Some(model.clone()));

        let defaulted = xml.replacen(" cancelActivity=\"true\"", "", 1);
        let (parsed_default, diagnostics) = import_xml(&defaulted);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(parsed_default, Some(model));
        let (invalid_boolean, diagnostics) = import_xml(&xml.replace("cancelActivity=\"true\"", "cancelActivity=\"sometimes\""));
        assert!(invalid_boolean.is_none());
        assert_eq!(diagnostics[0].element_id.as_deref(), Some("Boundary_A"));
        assert!(diagnostics[0].offset.is_some_and(|offset| offset < xml.len()));
        for malformed in [
            xml.replace("cancelActivity=\"true\"", "cancelActivity=\"sometimes\""),
            xml.replace("attachedToRef=\"Review_1\"", "attachedToRef=\"Missing_1\""),
            xml.replacen("</bpmn:timerEventDefinition>", "<bpmn:timeDuration>PT5S</bpmn:timeDuration></bpmn:timerEventDefinition>", 1),
        ] {
            let (parsed, diagnostics) = import_xml(&malformed);
            assert!(parsed.is_none());
            assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal && diagnostic.offset.is_some()), "{diagnostics:?}");
        }
    }

    #[test]
    fn timer_xml_round_trip_preserves_rules_timezone_and_di() {
        let cases = [
            ProcessTimerSpec::Date { at: "2027-01-02T03:04:05+01:00".into() },
            ProcessTimerSpec::Duration { seconds: 90_061 },
            ProcessTimerSpec::Cycle { seconds: 300, total_firings: Some(3) },
            ProcessTimerSpec::Daily { hour: 9, minute: 15, total_firings: None },
        ];
        for (index, rule) in cases.into_iter().enumerate() {
            let mut model = super::super::model::starter_model();
            model.timer_timezone = Some("Europe/Warsaw".into());
            model.variables.insert("threshold_value".into(), serde_json::json!({"nested_key": "Łódź & <ok>"}));
            model.nodes[0].kind = ProcessNodeKind::TimerStart { timer: rule.clone() };
            model.diagram.shapes.push(ProcessShape { element_id: "Start_1".into(), x: 12.0, y: 18.0, width: 36.0, height: 36.0 });
            if index < 2 {
                model.nodes.push(ProcessNode { id: "Wait_1".into(), name: "Wait & resume".into(), kind: ProcessNodeKind::TimerCatch { timer: rule } });
                model.sequence_flows[0].target_id = "Wait_1".into();
                model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_2".into(), source_id: "Wait_1".into(), target_id: "End_1".into(), condition: None });
            }
            let xml = export_xml(&model).unwrap();
            let (parsed, diagnostics) = import_xml(&xml);
            assert!(diagnostics.is_empty(), "{diagnostics:?}");
            assert_eq!(parsed, Some(model));
            if index == 1 {
                let expanded = xml.replace("PT90061S", "P1DT1H1M1S");
                let (expanded_model, diagnostics) = import_xml(&expanded);
                assert!(diagnostics.is_empty(), "{diagnostics:?}");
                assert_eq!(export_xml(&expanded_model.unwrap()).unwrap(), xml);
            }
        }
    }

    #[test]
    fn timer_xml_rejects_unsupported_profiles_duplicates_and_missing_timezone() {
        let mut model = super::super::model::starter_model();
        model.timer_timezone = Some("Europe/Warsaw".into());
        model.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Cycle { seconds: 300, total_firings: Some(3) } };
        let xml = export_xml(&model).unwrap();
        for invalid in [
            xml.replace("R3/PT300S", "R3/P1M"),
            xml.replace("R3/PT300S", "R3/P1D"),
            xml.replace("R3/PT300S", "R3/PT5M"),
            xml.replace("R3/PT300S", "R3/PT0.5S"),
            xml.replace("</bpmn:timerEventDefinition>", "<bpmn:timeDate>2027-01-01T00:00:00Z</bpmn:timeDate></bpmn:timerEventDefinition>"),
            xml.replace("</tentaflow:timerTimezone>", "</tentaflow:timerTimezone><tentaflow:timerTimezone>UTC</tentaflow:timerTimezone>"),
            xml.replace("<tentaflow:timerTimezone>Europe/Warsaw</tentaflow:timerTimezone>", ""),
        ] {
            let (parsed, diagnostics) = import_xml(&invalid);
            assert!(parsed.is_none());
            assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal && diagnostic.offset.is_some()), "{diagnostics:?}");
        }
    }

    #[test]
    fn supported_xml_round_trip_preserves_ids_and_di() {
        let mut model = super::super::model::starter_model();
        model
            .variables
            .insert("text".into(), serde_json::json!("a\rb"));
        model
            .variables
            .insert("text_crlf".into(), serde_json::json!("a\r\nb"));
        model.variables.insert(
            "threshold_value".into(),
            serde_json::json!({
                "nested_key": {"label": "R&D \"Łódź\" <check>", "enabled": true},
                "items": ["<first>", "second & third"],
                "limit": 3.5
            }),
        );
        model
            .variables
            .insert("xml_text".into(), serde_json::json!("<tag attr=\"&\">"));
        model.nodes.push(ProcessNode {
            id: "Review_1".into(),
            name: "R&D \"Łódź\"".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            },
        });
        model.nodes.push(ProcessNode {
            id: "Route_1".into(),
            name: "Route".into(),
            kind: ProcessNodeKind::ExclusiveGateway {
                default_flow_id: Some("Flow_4".into()),
            },
        });
        model.nodes.push(ProcessNode {
            id: "Service_1".into(),
            name: "Check".into(),
            kind: ProcessNodeKind::ServiceTask {
                flow_id: "flow-123".into(),
                input_mapping: BTreeMap::from([(
                    "condition_input".into(),
                    "vars.text == \"\"\"a\rb\"\"\"".into(),
                )]),
                output_mapping: BTreeMap::from([(
                    "condition_output".into(),
                    "vars.text_crlf == \"\"\"a\r\nb\"\"\"".into(),
                )]),
                verification: ActivityVerification::Condition {
                    expression:
                        "vars.text == \"\"\"a\rb\"\"\" && vars.text_crlf == \"\"\"a\r\nb\"\"\""
                            .into(),
                },
                timeout_seconds: 60,
                result_expression: None,
            },
        });
        model.sequence_flows[0].target_id = "Review_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_2".into(),
            source_id: "Review_1".into(),
            target_id: "Route_1".into(),
            condition: None,
        });
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_3".into(),
            source_id: "Route_1".into(),
            target_id: "Service_1".into(),
            condition: Some("vars.text == \"\"\"a\rb\"\"\"".into()),
        });
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_4".into(),
            source_id: "Route_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_5".into(),
            source_id: "Service_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        model.diagram.shapes.push(ProcessShape {
            element_id: "Start_1".into(),
            x: 10.0,
            y: 20.0,
            width: 36.0,
            height: 36.0,
        });
        model.diagram.edges.push(ProcessEdgeDiagram {
            sequence_flow_id: "Flow_1".into(),
            waypoints: vec![
                ProcessPoint { x: 46.0, y: 38.0 },
                ProcessPoint { x: 100.0, y: 38.0 },
            ],
        });
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("R&amp;D"));
        assert!(xml.contains("&amp;&amp;"));
        assert!(xml.contains("<tentaflow:variables>"));
        assert!(xml.contains("threshold_value"));
        assert!(xml.contains("&#13;"));
        let (parsed, diagnostics) = import_xml(&xml);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(parsed.as_ref(), Some(&model));
        for actual in [&model, parsed.as_ref().unwrap()] {
            let variables = actual
                .variables
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        crate::flow_engine::envelope::FlowValue::Json(value.clone()),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let payload = crate::flow_engine::envelope::FlowValue::Json(
                serde_json::to_value(&actual.variables).unwrap(),
            );
            let artifacts = HashMap::new();
            let meta = BTreeMap::new();
            let scope = crate::flow_engine::expr::ExprScope {
                vars: &variables,
                payload: &payload,
                artifacts: &artifacts,
                meta: &meta,
                extras: &[],
            };
            let service = actual
                .nodes
                .iter()
                .find(|node| node.id == "Service_1")
                .unwrap();
            let ProcessNodeKind::ServiceTask {
                input_mapping,
                output_mapping,
                verification: ActivityVerification::Condition { expression },
                ..
            } = &service.kind
            else {
                panic!("expected service condition");
            };
            for condition in [
                expression.as_str(),
                actual
                    .sequence_flows
                    .iter()
                    .find(|flow| flow.id == "Flow_3")
                    .and_then(|flow| flow.condition.as_deref())
                    .unwrap(),
                input_mapping["condition_input"].as_str(),
                output_mapping["condition_output"].as_str(),
            ] {
                assert!(crate::flow_engine::expr::evaluate_bool(condition, &scope, None).unwrap());
            }
        }
    }

    #[test]
    fn foreign_namespace_and_unsupported_element_are_rejected() {
        let xml = export_xml(&super::super::model::starter_model()).unwrap();
        assert!(import_xml(&xml.replace(BPMN, "https://evil.example/bpmn"))
            .0
            .is_none());
        assert!(
            import_xml(&xml.replace("<bpmn:endEvent", "<bpmn:scriptTask"))
                .0
                .is_none()
        );
        assert!(
            import_xml(&xml.replace("<bpmn:endEvent", "<!DOCTYPE bogus><bpmn:endEvent"))
                .0
                .is_none()
        );
    }

    #[test]
    fn duplicate_executable_extensions_fail_with_element_and_byte_diagnostics() {
        let mut model = super::super::model::starter_model();
        model.nodes.push(ProcessNode {
            id: "Service_1".into(),
            name: "Check".into(),
            kind: ProcessNodeKind::ServiceTask {
                flow_id: "flow-123".into(),
                input_mapping: BTreeMap::from([("input".into(), "vars.x".into())]),
                output_mapping: BTreeMap::from([("output".into(), "result.x".into())]),
                verification: ActivityVerification::Condition {
                    expression: "vars.x".into(),
                },
                timeout_seconds: 60,
                result_expression: None,
            },
        });
        model.sequence_flows[0].target_id = "Service_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_2".into(),
            source_id: "Service_1".into(),
            target_id: "End_1".into(),
            condition: None,
        });
        let xml = export_xml(&model).unwrap();
        assert_eq!(import_xml(&xml).0, Some(model));

        for (closing, extra, element) in [
            (
                "</tentaflow:inputMapping>",
                "<tentaflow:inputMapping>{}</tentaflow:inputMapping>",
                "inputMapping",
            ),
            (
                "</tentaflow:outputMapping>",
                "<tentaflow:outputMapping>{}</tentaflow:outputMapping>",
                "outputMapping",
            ),
            (
                "</tentaflow:condition>",
                "<tentaflow:condition>vars.y</tentaflow:condition>",
                "condition",
            ),
            (
                "</bpmn:extensionElements>",
                "<bpmn:extensionElements/>",
                "extensionElements",
            ),
        ] {
            assert!(xml.contains(closing));
            let invalid = xml.replacen(closing, &format!("{closing}{extra}"), 1);
            let (parsed, diagnostics) = import_xml(&invalid);
            assert!(parsed.is_none(), "{element} was silently accepted");
            assert_eq!(diagnostics.len(), 1);
            let diagnostic = &diagnostics[0];
            assert!(diagnostic.fatal);
            assert!(diagnostic.message.contains("duplicate element"));
            assert!(diagnostic.message.contains(element));
            assert!(diagnostic
                .offset
                .is_some_and(|offset| offset < invalid.len()));
        }
    }

    #[test]
    fn process_variables_reject_duplicate_non_object_malformed_and_oversized_json() {
        let xml = export_xml(&super::super::model::starter_model()).unwrap();
        let original = "<tentaflow:variables>{}</tentaflow:variables>";
        assert!(xml.contains(original));
        let cases = [
            (
                format!("{original}<tentaflow:variables>{{}}</tentaflow:variables>"),
                "variables",
            ),
            (
                "<tentaflow:variables>[1]</tentaflow:variables>".into(),
                "variables",
            ),
            (
                "<tentaflow:variables>{</tentaflow:variables>".into(),
                "variables",
            ),
            (
                format!(
                    "<tentaflow:variables>{}</tentaflow:variables>",
                    "x".repeat(MAX_VARIABLE_BYTES + 1)
                ),
                "variables",
            ),
        ];
        for (replacement, element) in cases {
            let invalid = xml.replacen(original, &replacement, 1);
            let (parsed, diagnostics) = import_xml(&invalid);
            assert!(parsed.is_none(), "{element} was silently accepted");
            assert_eq!(diagnostics.len(), 1);
            assert!(diagnostics[0].fatal);
            assert!(diagnostics[0].message.contains(element));
            assert!(diagnostics[0]
                .offset
                .is_some_and(|offset| offset < invalid.len()));
        }

        let duplicate_extension = xml.replacen(
            "</bpmn:extensionElements>",
            "</bpmn:extensionElements><bpmn:extensionElements><tentaflow:variables>{}</tentaflow:variables></bpmn:extensionElements>",
            1,
        );
        let (parsed, diagnostics) = import_xml(&duplicate_extension);
        assert!(parsed.is_none());
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].fatal);
        assert!(diagnostics[0].message.contains("extensionElements"));
        assert_eq!(diagnostics[0].element_id.as_deref(), Some("Process_1"));
        assert!(diagnostics[0].offset.is_some());
    }
    #[test]
    fn embedded_subprocess_xml_round_trip_partitions_di_and_rejects_expansion() {
        let mut model = super::super::model::starter_model();
        model.nodes.insert(1, ProcessNode {
            id: "Sub_1".into(), name: "Review & scope".into(),
            kind: ProcessNodeKind::SubProcess {
                body: ProcessSubProcess {
                    nodes: vec![
                        ProcessNode { id: "LocalStart".into(), name: "Local start".into(), kind: ProcessNodeKind::Start },
                        ProcessNode { id: "LocalEnd".into(), name: "Local end".into(), kind: ProcessNodeKind::End },
                    ],
                    sequence_flows: vec![ProcessSequenceFlow {
                        id: "LocalFlow".into(), source_id: "LocalStart".into(),
                        target_id: "LocalEnd".into(), condition: None,
                    }],
                    variables: BTreeMap::from([("local_ID".into(), serde_json::json!("A & B"))]),
                    diagram: ProcessDiagram {
                        shapes: vec![ProcessShape {
                            element_id: "LocalStart".into(), x: 30.0, y: 40.0,
                            width: 36.0, height: 36.0,
                        }],
                        edges: vec![ProcessEdgeDiagram {
                            sequence_flow_id: "LocalFlow".into(),
                            waypoints: vec![ProcessPoint { x: 66.0, y: 58.0 }, ProcessPoint { x: 130.0, y: 58.0 }],
                        }],
                    },
                },
                input_mapping: BTreeMap::from([("local_ID".into(), "vars.source_ID".into())]),
                output_mapping: BTreeMap::from([("result_ID".into(), "outputs.local_ID".into())]),
            },
        });
        model.sequence_flows[0].target_id = "Sub_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_2".into(), source_id: "Sub_1".into(), target_id: "End_1".into(), condition: None,
        });
        model.diagram.shapes.push(ProcessShape {
            element_id: "Sub_1".into(), x: 120.0, y: 80.0, width: 160.0, height: 100.0,
        });
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<bpmn:subProcess id=\"Sub_1\""));
        assert!(xml.contains("bpmnElement=\"Sub_1\" isExpanded=\"false\""));
        assert_eq!(import_xml(&xml).0, Some(model.clone()));
        let expanded = xml.replacen("isExpanded=\"false\"", "isExpanded=\"true\"", 1);
        let (parsed, diagnostics) = import_xml(&expanded);
        assert!(parsed.is_none());
        assert_eq!(diagnostics[0].element_id.as_deref(), Some("Sub_1"));
        assert!(diagnostics[0].offset.is_some());
        let event_subprocess = xml.replacen("<bpmn:subProcess id=\"Sub_1\"",
            "<bpmn:subProcess triggeredByEvent=\"true\" id=\"Sub_1\"", 1);
        assert!(import_xml(&event_subprocess).0.is_none());
    }

}
