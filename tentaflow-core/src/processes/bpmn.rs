// ============ File: bpmn.rs — bounded BPMN B1 XML import and export ============

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{bail, ensure, Context, Result};
use quick_xml::events::Event;
use quick_xml::name::{QName, ResolveResult};
use quick_xml::{NsReader, XmlVersion};
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessCalendarPin, ProcessCallableReference, ProcessDiagnostic,
    ProcessDiagram, ProcessEdgeDiagram, ProcessErrorDeclaration, ProcessEscalationDeclaration,
    ProcessMessageDeclaration, ProcessMessageTargetSpec, ProcessModel, ProcessMultiInstanceInput,
    ProcessMultiInstanceMode, ProcessNode, ProcessNodeKind, ProcessPoint, ProcessRepeatSpec,
    ProcessSignalDeclaration,
    ProcessSequenceFlow, ProcessShape, ProcessSubProcess, ProcessTimerSpec, ProcessWorkCalendar,
};

use super::model::{
    validate_model, validate_timer_spec, validate_variables, EscalationPathError,
    EventGatewayProfileError, MAX_MODEL_BYTES,
    MAX_VARIABLE_BYTES,
};

const BPMN: &str = "http://www.omg.org/spec/BPMN/20100524/MODEL";
const BPMNDI: &str = "http://www.omg.org/spec/BPMN/20100524/DI";
const DC: &str = "http://www.omg.org/spec/DD/20100524/DC";
const DI: &str = "http://www.omg.org/spec/DD/20100524/DI";
const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const TF: &str = "https://tentaflow.app/bpmn/1";
const GRAPH_ELEMENTS: [(&str, &str); 19] = [
    (BPMN, "extensionElements"), (BPMN, "startEvent"),
    (BPMN, "intermediateCatchEvent"), (BPMN, "intermediateThrowEvent"),
    (BPMN, "boundaryEvent"), (BPMN, "endEvent"), (BPMN, "userTask"),
    (BPMN, "serviceTask"), (BPMN, "scriptTask"), (BPMN, "manualTask"), (BPMN, "sendTask"), (BPMN, "receiveTask"),
    (BPMN, "subProcess"), (BPMN, "callActivity"),
    (BPMN, "exclusiveGateway"), (BPMN, "eventBasedGateway"),
    (BPMN, "parallelGateway"), (BPMN, "inclusiveGateway"), (BPMN, "sequenceFlow"),
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
    fn find_id(&self, id: &str) -> Option<&Element> {
        if self.attr("id") == Some(id) {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find_id(id))
    }
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
        if (name == "signalRef" && uri != namespace) || (!uri.is_empty() && uri != namespace) {
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
                    || (matches!(self.local.as_str(), "conditionExpression" | "loopCardinality" | "loopCondition" | "from")
                        && self.ns == BPMN && ns == XSI && name == "type"),
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
                    if attr_ns.is_empty()
                        && matches!(
                            attr_local.as_str(),
                            "messageRef" | "errorRef" | "escalationRef" | "signalRef" | "calledElement"
                        )
                    {
                        let invalid_qname = |reason: &str| XmlElementError {
                            message: format!("{reason} at byte {offset}"),
                            element_id: stack
                                .last()
                                .and_then(|parent| parent.attr("id"))
                                .map(str::to_string),
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
                if stack.last().is_some_and(|element| element.is(BPMN, "loopDataInputRef")) {
                    let reference = stack.last().expect("open loopDataInputRef");
                    let value = reference.text.trim();
                    let reference_offset = reference.offset;
                    let activity_id = stack.iter().rev().find_map(|element| element.attr("id"))
                        .map(str::to_string);
                    let (resolved, local) = reader.resolver().resolve_element(QName(value.as_bytes()));
                    let uri = ns_text(resolved).map_err(|error| XmlElementError {
                        message: format!("invalid loop data input QName at byte {reference_offset}: {error}"),
                        element_id: activity_id.clone(), offset: reference_offset,
                    })?;
                    let local = String::from_utf8(local.as_ref().to_vec())?;
                    if value.is_empty() || !local.bytes().next().is_some_and(|byte| byte.is_ascii_alphabetic())
                        || !local.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')) {
                        return Err(XmlElementError {
                            message: format!("invalid loop data input QName at byte {reference_offset}"),
                            element_id: activity_id, offset: reference_offset,
                        }.into());
                    }
                    stack.last_mut().expect("open loopDataInputRef").qnames.insert("text".into(), (uri, local));
                }
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalThrowConfig {
    payload_expression: String,
    ttl_seconds: u32,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalCatchConfig {
    output_mapping: BTreeMap<String, String>,
}

fn message_configuration<T: serde::de::DeserializeOwned>(element: &Element, marker: &str) -> Result<T> {
    let extension = element.child(BPMN, "extensionElements")?
        .ok_or_else(|| XmlElementError { message: format!("{} requires TentaFlow {marker} marker at byte {}", element.local, element.offset),
            element_id: element.attr("id").map(str::to_string), offset: element.offset })?;
    extension.attrs_only(&[]).map_err(|error| XmlElementError {
        message: format!("invalid message extension at byte {}: {error}", extension.offset),
        element_id: element.attr("id").map(str::to_string), offset: extension.offset,
    })?;
    if extension.children.len() != 1 || !extension.children[0].is(TF, marker) {
        let offending = extension.children.iter().find(|child| !child.is(TF, marker))
            .or_else(|| extension.children.get(1)).unwrap_or(extension);
        return Err(XmlElementError {
            message: format!("{} requires exactly one TentaFlow {marker} marker at byte {}", element.local, offending.offset),
            element_id: element.attr("id").map(str::to_string), offset: offending.offset,
        }.into());
    }
    let config = &extension.children[0];
    config.attrs_only(&[]).map_err(|error| XmlElementError {
        message: format!("invalid {marker} marker at byte {}: {error}", config.offset),
        element_id: element.attr("id").map(str::to_string), offset: config.offset,
    })?;
    if !config.children.is_empty() {
        return Err(XmlElementError { message: format!("{marker} config must contain JSON text at byte {}", config.offset),
            element_id: element.attr("id").map(str::to_string), offset: config.offset }.into());
    }
    serde_json::from_str(config.text.trim()).map_err(|error| XmlElementError {
        message: format!("invalid {marker} config at byte {}: {error}", config.offset),
        element_id: element.attr("id").map(str::to_string),
        offset: config.offset,
    }.into())
}

fn event_reference(element: &Element, kind: &str, namespace: &str) -> Result<String> {
    let definition = element
        .child(BPMN, kind)?
        .context("event definition is required")?;
    let attribute = match kind {
        "messageEventDefinition" => "messageRef",
        "escalationEventDefinition" => "escalationRef",
        "signalEventDefinition" => "signalRef",
        _ => "errorRef",
    };
    definition.attrs_only(&[attribute])?;
    ensure!(
        definition.children.is_empty() && definition.text.trim().is_empty(),
        "event definition cannot contain other content at byte {}",
        definition.offset
    );
    definition.reference(attribute, namespace)
}

fn xml_boolean(value: Option<&str>, default: bool, element: &Element, name: &str) -> Result<bool> {
    match value {
        None => Ok(default),
        Some("true" | "1") => Ok(true),
        Some("false" | "0") => Ok(false),
        Some(_) => bail!("invalid {name} at byte {}", element.offset),
    }
}

fn repeat_expression(element: &Element) -> Result<String> {
    element.attrs_only(&["language"])?;
    ensure!(element.attrs.contains_key(&(XSI.to_string(), "type".to_string()))
        && element.attr("language") == Some("https://cel.dev/spec")
        && element.children.is_empty(),
        "unsupported {} expression at byte {}", element.local, element.offset);
    Ok(element.text.clone())
}

fn repeat_from_xml(element: &Element, target_namespace: &str) -> Result<Option<ProcessRepeatSpec>> {
    let mi = element.child(BPMN, "multiInstanceLoopCharacteristics")?;
    let standard = element.child(BPMN, "standardLoopCharacteristics")?;
    if let (Some(mi), Some(standard)) = (mi, standard) {
        let offending = if mi.offset > standard.offset { mi } else { standard };
        return Err(XmlElementError { message: format!("mixed repeat characteristics at byte {}", offending.offset),
            element_id: element.attr("id").map(str::to_string), offset: offending.offset }.into());
    }
    let extension = element.child(BPMN, "extensionElements")?;
    let binding = extension.map(|ext| ext.child(TF, "repeat")).transpose()?.flatten();
    if mi.is_none() && standard.is_none() {
        ensure!(binding.is_none(), "repeat extension lacks loop characteristics at byte {}", element.offset);
        ensure!(element.children.iter().all(|child| child.is(BPMN, "extensionElements")),
            "activity IO requires loop characteristics at byte {}", element.offset);
        return Ok(None);
    }
    ensure!(element.text.trim().is_empty(), "activity repeat has unsupported text at byte {}", element.offset);
    let binding = binding.context("loop characteristics require TentaFlow repeat binding")?;
    binding.attrs_only(&["outputCollectionVariable"])?;
    ensure!(binding.children.is_empty() && binding.text.trim().is_empty(),
        "repeat binding must be empty at byte {}", binding.offset);
    let output_collection_variable = binding.required("outputCollectionVariable")?;
    let mut previous_rank = 0;
    for child in &element.children {
        let rank = match child.local.as_str() {
            "extensionElements" if child.ns == BPMN => 1,
            "ioSpecification" if child.ns == BPMN => 2,
            "dataInputAssociation" if child.ns == BPMN => 3,
            "multiInstanceLoopCharacteristics" | "standardLoopCharacteristics" if child.ns == BPMN => 4,
            _ => 5,
        };
        ensure!(rank > previous_rank && rank < 5,
            "unsupported activity repeat child order at byte {}", child.offset);
        previous_rank = rank;
    }
    if let Some(mi) = mi {
        mi.attrs_only(&["isSequential", "behavior"])?;
        ensure!(mi.text.trim().is_empty(), "multi-instance characteristic has unsupported text at byte {}", mi.offset);
        ensure!(mi.attr("behavior").is_none_or(|value| value == "All"),
            "unsupported multi-instance behavior at byte {}", mi.offset);
        mi.children_only(&[(BPMN, "loopCardinality"), (BPMN, "loopDataInputRef"), (BPMN, "inputDataItem")])?;
        let mode = if xml_boolean(mi.attr("isSequential"), false, mi, "isSequential")? {
            ProcessMultiInstanceMode::Sequential
        } else { ProcessMultiInstanceMode::Parallel };
        let input = if let Some(cardinality) = mi.child(BPMN, "loopCardinality")? {
            ensure!(mi.children.len() == 1, "cardinality repeat has unsupported children at byte {}", mi.offset);
            let literal = repeat_expression(cardinality)?;
            ensure!(!literal.is_empty() && literal.bytes().all(|byte| byte.is_ascii_digit()),
                "repeat cardinality must be a bounded integer at byte {}", cardinality.offset);
            let count = literal.parse().map_err(|error| XmlElementError {
                message: format!("invalid repeat cardinality at byte {}: {error}", cardinality.offset),
                element_id: element.attr("id").map(str::to_string), offset: cardinality.offset,
            })?;
            if count > 16 {
                return Err(XmlElementError {
                    message: format!("repeat cardinality exceeds 16 at byte {}", cardinality.offset),
                    element_id: element.attr("id").map(str::to_string), offset: cardinality.offset,
                }.into());
            }
            ProcessMultiInstanceInput::Cardinality { count }
        } else {
            ensure!(mi.children.len() == 2, "collection repeat requires an input ref and item at byte {}", mi.offset);
            ensure!(mi.children[0].is(BPMN, "loopDataInputRef")
                && mi.children[1].is(BPMN, "inputDataItem"),
                "collection repeat children are out of order at byte {}", mi.offset);
            let reference = mi.child(BPMN, "loopDataInputRef")?.context("collection repeat lacks loopDataInputRef")?;
            reference.attrs_only(&[])?;
            ensure!(reference.children.is_empty(), "loopDataInputRef must be QName text");
            let (uri, data_id) = reference.qnames.get("text").context("loopDataInputRef requires a QName")?;
            ensure!(uri == target_namespace, "foreign loopDataInputRef QName at byte {}", reference.offset);
            let item = mi.child(BPMN, "inputDataItem")?.context("collection repeat lacks inputDataItem")?;
            item.attrs_only(&["id", "isCollection"])?;
            ensure!(item.attr("id").is_some() && !xml_boolean(item.attr("isCollection"), false, item, "isCollection")?,
                "invalid repeat inputDataItem at byte {}", item.offset);
            ensure!(item.children.is_empty() && item.text.trim().is_empty(),
                "repeat inputDataItem must be empty at byte {}", item.offset);
            let io = element.child(BPMN, "ioSpecification")?.context("collection repeat requires ioSpecification")?;
            io.attrs_only(&[])?;
            ensure!(io.text.trim().is_empty(), "repeat ioSpecification has unsupported text at byte {}", io.offset);
            io.children_only(&[(BPMN, "dataInput"), (BPMN, "inputSet"), (BPMN, "outputSet")])?;
            ensure!(io.children.len() == 3, "collection repeat requires one input and two sets");
            ensure!(io.children[0].is(BPMN, "dataInput")
                && io.children[1].is(BPMN, "inputSet")
                && io.children[2].is(BPMN, "outputSet"),
                "collection ioSpecification children are out of order at byte {}", io.offset);
            let data = io.child(BPMN, "dataInput")?.context("collection input is missing")?;
            data.attrs_only(&["id", "isCollection"])?;
            ensure!(data.attr("id") == Some(data_id.as_str())
                && xml_boolean(data.attr("isCollection"), false, data, "isCollection")?
                && data.children.is_empty() && data.text.trim().is_empty(),
                "collection dataInput does not match QName at byte {}", data.offset);
            let set = io.child(BPMN, "inputSet")?.context("repeat inputSet is missing")?;
            set.attrs_only(&["id"])?;
            set.children_only(&[(BPMN, "dataInputRefs")])?;
            ensure!(set.text.trim().is_empty() && set.children.len() == 1
                && set.children[0].text.trim() == data_id.as_str(),
                "repeat inputSet does not select collection input at byte {}", set.offset);
            let output_set = io.child(BPMN, "outputSet")?.context("repeat outputSet is missing")?;
            output_set.attrs_only(&["id"])?;
            ensure!(output_set.children.is_empty() && output_set.text.trim().is_empty(),
                "repeat outputSet must be empty at byte {}", output_set.offset);
            let association = element.child(BPMN, "dataInputAssociation")?.context("repeat input association is missing")?;
            association.attrs_only(&["id"])?;
            ensure!(association.text.trim().is_empty(),
                "repeat association has unsupported text at byte {}", association.offset);
            association.children_only(&[(BPMN, "targetRef"), (BPMN, "assignment")])?;
            ensure!(association.children.len() == 2
                && association.children[0].is(BPMN, "targetRef")
                && association.children[1].is(BPMN, "assignment")
                && association.child(BPMN, "targetRef")?.is_some_and(|target| target.children.is_empty() && target.text.trim() == data_id.as_str()),
                "repeat association target differs from collection input at byte {}", association.offset);
            let assignment = association.child(BPMN, "assignment")?.context("repeat assignment is missing")?;
            assignment.attrs_only(&["id"])?;
            ensure!(assignment.text.trim().is_empty(), "repeat assignment has unsupported text at byte {}", assignment.offset);
            assignment.children_only(&[(BPMN, "from"), (BPMN, "to")])?;
            ensure!(assignment.children.len() == 2
                && assignment.children[0].is(BPMN, "from")
                && assignment.children[1].is(BPMN, "to")
                && assignment.child(BPMN, "to")?.is_some_and(|to| to.children.is_empty() && to.text.trim() == data_id.as_str()),
                "repeat assignment does not target collection input at byte {}", assignment.offset);
            let from = assignment.child(BPMN, "from")?.context("repeat collection expression is missing")?;
            ProcessMultiInstanceInput::CollectionExpression { expression: repeat_expression(from)? }
        };
        ensure!(matches!(input, ProcessMultiInstanceInput::CollectionExpression { .. })
            || (element.child(BPMN, "ioSpecification")?.is_none()
                && element.child(BPMN, "dataInputAssociation")?.is_none()),
            "cardinality repeat cannot define collection IO at byte {}", element.offset);
        Ok(Some(ProcessRepeatSpec::MultiInstance { mode, input, output_collection_variable }))
    } else {
        let standard = standard.context("structured loop characteristic is missing")?;
        standard.attrs_only(&["testBefore", "loopMaximum"])?;
        ensure!(standard.text.trim().is_empty(), "structured loop has unsupported text at byte {}", standard.offset);
        standard.children_only(&[(BPMN, "loopCondition")])?;
        ensure!(standard.children.len() == 1
            && element.child(BPMN, "ioSpecification")?.is_none()
            && element.child(BPMN, "dataInputAssociation")?.is_none(),
            "structured loop has unsupported IO or child at byte {}", standard.offset);
        let condition = repeat_expression(standard.child(BPMN, "loopCondition")?.context("loop condition is missing")?)?;
        let max_iterations = standard.required("loopMaximum")?.parse().map_err(|error| XmlElementError {
            message: format!("invalid loop maximum at byte {}: {error}", standard.offset),
            element_id: element.attr("id").map(str::to_string), offset: standard.offset,
        })?;
        if !(1..=32).contains(&max_iterations) {
            return Err(XmlElementError {
                message: format!("loop maximum outside 1..=32 at byte {}", standard.offset),
                element_id: element.attr("id").map(str::to_string), offset: standard.offset,
            }.into());
        }
        let test_before = xml_boolean(standard.attr("testBefore"), false, standard, "testBefore")?;
        Ok(Some(ProcessRepeatSpec::StructuredLoop { condition, test_before, max_iterations, output_collection_variable }))
    }
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
            if let Some(definition) = element.child(BPMN, "signalEventDefinition")? {
                return Err(XmlElementError { message: format!("signal start is unsupported at byte {}", definition.offset),
                    element_id: Some(id.clone()), offset: definition.offset }.into());
            }
            element.children_only(&[(BPMN, "timerEventDefinition"), (BPMN, "messageEventDefinition"), (BPMN, "extensionElements")])?;
            if element.children.is_empty() { ProcessNodeKind::Start }
            else if element.child(BPMN, "timerEventDefinition")?.is_some() {
                ensure!(element.children.len() == 1, "timer start cannot have another event definition");
                ProcessNodeKind::TimerStart { timer: parsed_timer()? }
            } else {
                ensure!(element.children.len() == 2, "message start requires one definition and one config");
                let config: MessageStartConfig = message_configuration(element, "message")?;
                ProcessNodeKind::MessageStart {
                    message_ref: event_reference(element, "messageEventDefinition", target_namespace)?,
                    output_mapping: config.output_mapping,
                }
            }
        }
        "intermediateCatchEvent" => {
            element.attrs_only(&["id", "name"])?;
            element.children_only(&[(BPMN, "timerEventDefinition"), (BPMN, "messageEventDefinition"),
                (BPMN, "signalEventDefinition"), (BPMN, "extensionElements")])?;
            if element.child(BPMN, "timerEventDefinition")?.is_some() {
                ensure!(element.children.len() == 1, "timer catch cannot have another event definition");
                ProcessNodeKind::TimerCatch { timer: parsed_timer()? }
            } else if let Some(definition) = element.child(BPMN, "signalEventDefinition")? {
                ensure!(element.children.len() == 2, "signal catch requires one definition and one config");
                let config: SignalCatchConfig = message_configuration(element, "signalCatch")?;
                let signal_ref = event_reference(element, "signalEventDefinition", target_namespace)
                    .map_err(|error| XmlElementError { message: error.to_string(),
                        element_id: Some(id.clone()),
                        offset: definition.offset })?;
                ProcessNodeKind::SignalCatch { signal_ref, output_mapping: config.output_mapping }
            } else {
                ensure!(element.children.len() == 2, "message catch requires one definition and one config");
                let config: MessageCatchConfig = message_configuration(element, "message")?;
                ProcessNodeKind::MessageCatch {
                    message_ref: event_reference(element, "messageEventDefinition", target_namespace)?,
                    correlation_expression: config.correlation_expression,
                    output_mapping: config.output_mapping,
                }
            }
        }
        "intermediateThrowEvent" => {
            element.attrs_only(&["id", "name"])?;
            element.children_only(&[(BPMN, "messageEventDefinition"),
                (BPMN, "signalEventDefinition"), (BPMN, "extensionElements")])?;
            if let Some(definition) = element.child(BPMN, "signalEventDefinition")? {
                ensure!(element.children.len() == 2, "signal throw requires one definition and one config");
                let config: SignalThrowConfig = message_configuration(element, "signalThrow")?;
                let signal_ref = event_reference(element, "signalEventDefinition", target_namespace)
                    .map_err(|error| XmlElementError { message: error.to_string(),
                        element_id: Some(id.clone()),
                        offset: definition.offset })?;
                ProcessNodeKind::SignalThrow { signal_ref,
                    payload_expression: config.payload_expression, ttl_seconds: config.ttl_seconds }
            } else {
                ensure!(element.children.len() == 2, "message throw requires one definition and one config");
                let config: MessageThrowConfig = message_configuration(element, "message")?;
                ProcessNodeKind::MessageThrow {
                    message_ref: event_reference(element, "messageEventDefinition", target_namespace)?,
                    target: config.target,
                    correlation_expression: config.correlation_expression,
                    payload_expression: config.payload_expression,
                    ttl_seconds: config.ttl_seconds,
                }
            }
        }
        "boundaryEvent" => {
            element.attrs_only(&["id", "name", "attachedToRef", "cancelActivity"])?;
            if let Some(definition) = element.child(BPMN, "signalEventDefinition")? {
                return Err(XmlElementError { message: format!("boundary signal is unsupported at byte {}", definition.offset),
                    element_id: Some(id.clone()), offset: definition.offset }.into());
            }
            element.children_only(&[
                (BPMN, "timerEventDefinition"),
                (BPMN, "messageEventDefinition"),
                (BPMN, "errorEventDefinition"),
                (BPMN, "escalationEventDefinition"),
                (BPMN, "extensionElements"),
            ])?;
            let cancel_activity = match element.attr("cancelActivity") {
                None | Some("true" | "1") => true,
                Some("false" | "0") => false,
                Some(other) => {
                    return Err(XmlElementError {
                        message: format!(
                            "invalid boundary cancelActivity {other} at byte {}",
                            element.offset
                        ),
                        element_id: Some(id.clone()),
                        offset: element.offset,
                    }
                    .into())
                }
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
                let config: MessageCatchConfig = message_configuration(element, "message")?;
                ProcessNodeKind::BoundaryMessage {
                    attached_to_id: element.required("attachedToRef")?,
                    cancel_activity,
                    message_ref: event_reference(
                        element,
                        "messageEventDefinition",
                        target_namespace,
                    )?,
                    correlation_expression: config.correlation_expression,
                    output_mapping: config.output_mapping,
                }
            } else if let Some(definition) = element.child(BPMN, "escalationEventDefinition")? {
                ensure!(
                    element.children.len() <= 2,
                    "boundary escalation has unsupported event definitions"
                );
                ensure!(
                    element
                        .children
                        .iter()
                        .all(|child| child.is(BPMN, "escalationEventDefinition")
                            || child.is(BPMN, "extensionElements")),
                    "boundary escalation cannot contain another event definition"
                );
                definition.attrs_only(&["escalationRef"])?;
                ensure!(
                    definition.children.is_empty() && definition.text.trim().is_empty(),
                    "escalation definition must be empty"
                );
                let output_mapping =
                    if let Some(extension) = element.child(BPMN, "extensionElements")? {
                        extension.attrs_only(&[])?;
                        extension.children_only(&[(TF, "outputMapping")])?;
                        ensure!(
                            extension.children.len() == 1,
                            "boundary escalation extension requires outputMapping"
                        );
                        mapping(extension, "outputMapping")?
                    } else {
                        BTreeMap::new()
                    };
                ProcessNodeKind::BoundaryEscalation {
                    attached_to_id: element.required("attachedToRef")?,
                    cancel_activity,
                    escalation_ref: if definition.attr("escalationRef").is_some() {
                        Some(definition.reference("escalationRef", target_namespace)?)
                    } else {
                        None
                    },
                    output_mapping,
                }
            } else {
                ensure!(
                    cancel_activity,
                    "boundary error must interrupt its activity"
                );
                ensure!(
                    element.child(BPMN, "timerEventDefinition")?.is_none()
                        && element.child(BPMN, "messageEventDefinition")?.is_none(),
                    "boundary error cannot contain another event definition"
                );
                let definition = element
                    .child(BPMN, "errorEventDefinition")?
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
            if let Some(child) = element.children.iter().find(|child|
                !child.is(BPMN, "errorEventDefinition") && !child.is(BPMN, "terminateEventDefinition")) {
                return Err(XmlElementError {
                    message: format!("unsupported end event definition {{{}}}{} at byte {}",
                        child.ns, child.local, child.offset),
                    element_id: Some(id.clone()), offset: child.offset,
                }.into());
            }
            let terminate_definitions = element.children.iter()
                .filter(|child| child.ns == BPMN && child.local == "terminateEventDefinition")
                .collect::<Vec<_>>();
            if let Some(definition) = terminate_definitions.first() {
                if element.children.len() != 1 || terminate_definitions.len() != 1 {
                    let offending = element.children.get(1).unwrap_or(*definition);
                    return Err(XmlElementError {
                        message: format!("terminate end {} requires exactly one event definition at byte {}", id, offending.offset),
                        element_id: Some(id.clone()), offset: offending.offset,
                    }.into());
                }
                if let Err(error) = definition.attrs_only(&[]) {
                    return Err(XmlElementError {
                        message: format!("invalid terminate definition for {} at byte {}: {error}", id, definition.offset),
                        element_id: Some(id.clone()), offset: definition.offset,
                    }.into());
                }
                if !definition.children.is_empty() || !definition.text.trim().is_empty() {
                    return Err(XmlElementError {
                        message: format!("terminate definition for {} must be empty at byte {}", id, definition.offset),
                        element_id: Some(id.clone()), offset: definition.offset,
                    }.into());
                }
                ProcessNodeKind::TerminateEnd
            } else if element.child(BPMN, "errorEventDefinition")?.is_some() {
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
        "inclusiveGateway" => {
            element.attrs_only(&["id", "name", "default", "gatewayDirection"])?;
            element.children_only(&[])?;
            ProcessNodeKind::InclusiveGateway {
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
            element.children_only(&[(BPMN, "extensionElements"), (BPMN, "ioSpecification"),
                (BPMN, "dataInputAssociation"), (BPMN, "multiInstanceLoopCharacteristics"),
                (BPMN, "standardLoopCharacteristics")])?;
            let ext = element.child(BPMN, "extensionElements")?;
            let user = if let Some(ext) = ext {
                ext.attrs_only(&[])?;
                ext.children_only(&[(TF, "user"), (TF, "repeat")])?;
                ensure!(ext.children.len() == usize::from(ext.child(TF, "user")?.is_some())
                    + usize::from(ext.child(TF, "repeat")?.is_some()),
                    "user task has unsupported TentaFlow extensions");
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
        "manualTask" => {
            element.attrs_only(&["id", "name"])?;
            for child in &element.children {
                if !child.is(BPMN, "documentation") && !child.is(BPMN, "extensionElements") {
                    return Err(XmlElementError { message: format!("unsupported manual task child at byte {}", child.offset),
                        element_id: Some(id.clone()), offset: child.offset }.into());
                }
            }
            if !element.text.trim().is_empty() {
                return Err(XmlElementError { message: format!("manual task {} has unsupported text at byte {}", id, element.offset),
                    element_id: Some(id.clone()), offset: element.offset }.into());
            }
            let documentation = element.child(BPMN, "documentation")?;
            let instructions = if let Some(doc) = documentation {
                doc.attrs_only(&["textFormat"])?;
                if doc.attr("textFormat").is_some_and(|format| format != "text/plain") || !doc.children.is_empty() {
                    return Err(XmlElementError { message: format!("invalid manual documentation at byte {}", doc.offset),
                        element_id: Some(id.clone()), offset: doc.offset }.into());
                }
                doc.text.clone()
            } else { String::new() };
            let ext = element.child(BPMN, "extensionElements")?.ok_or_else(|| XmlElementError {
                message: format!("manual task {} requires TentaFlow manual marker at byte {}", id, element.offset),
                element_id: Some(id.clone()), offset: element.offset,
            })?;
            if element.children.last().is_none_or(|last| last.is(BPMN, "documentation")) {
                let offending = element.children.last().unwrap_or(ext);
                return Err(XmlElementError { message: format!("manual task {} has invalid child order at byte {}", id, offending.offset),
                    element_id: Some(id.clone()), offset: offending.offset }.into());
            }
            ext.attrs_only(&[])?;
            if ext.children.len() != 1 || !ext.children[0].is(TF, "manual") {
                let offending = ext.children.iter().find(|child| !child.is(TF, "manual"))
                    .or_else(|| ext.children.get(1)).unwrap_or(ext);
                return Err(XmlElementError { message: format!("manual task {} requires exactly one TentaFlow manual marker at byte {}", id, offending.offset),
                    element_id: Some(id.clone()), offset: offending.offset }.into());
            }
            let marker = &ext.children[0];
            if let Err(error) = marker.attrs_only(&["assigneeUserId"]) {
                return Err(XmlElementError { message: format!("invalid manual marker at byte {}: {error}", marker.offset),
                    element_id: Some(id.clone()), offset: marker.offset }.into());
            }
            if !marker.children.is_empty() || !marker.text.trim().is_empty() {
                return Err(XmlElementError { message: format!("manual marker has unsupported content at byte {}", marker.offset),
                    element_id: Some(id.clone()), offset: marker.offset }.into());
            }
            ProcessNodeKind::ManualTask { assignee_user_id: marker.attr("assigneeUserId").map(str::to_string), instructions }
        }
        "sendTask" | "receiveTask" => {
            let receive = element.local == "receiveTask";
            element.attrs_only(if receive {
                &["id", "name", "messageRef", "implementation", "instantiate"]
            } else {
                &["id", "name", "messageRef", "implementation"]
            }).map_err(|error| XmlElementError { message: format!("unsupported {} attribute at byte {}: {error}", element.local, element.offset),
                element_id: Some(id.clone()), offset: element.offset })?;
            if element.attr("implementation") != Some("##unspecified") {
                return Err(XmlElementError { message: format!("{} {} requires implementation ##unspecified at byte {}", element.local, id, element.offset),
                    element_id: Some(id.clone()), offset: element.offset }.into());
            }
            if receive && xml_boolean(element.attr("instantiate"), false, element, "instantiate")? {
                return Err(XmlElementError { message: format!("receive task cannot instantiate at byte {}", element.offset),
                    element_id: Some(id.clone()), offset: element.offset }.into());
            }
            if !element.text.trim().is_empty() {
                return Err(XmlElementError { message: format!("{} has unsupported text at byte {}", element.local, element.offset),
                    element_id: Some(id.clone()), offset: element.offset }.into());
            }
            for child in &element.children {
                if !child.is(BPMN, "extensionElements") {
                    return Err(XmlElementError { message: format!("unsupported {} child at byte {}", element.local, child.offset),
                        element_id: Some(id.clone()), offset: child.offset }.into());
                }
            }
            let marker = if receive { "receiveTask" } else { "sendTask" };
            let message_ref = element.reference("messageRef", target_namespace)?;
            if !element.qnames.get("messageRef").is_some_and(|(uri, _)| uri == target_namespace) {
                return Err(XmlElementError { message: format!("{} requires target-namespace messageRef QName at byte {}", element.local, element.offset),
                    element_id: Some(id.clone()), offset: element.offset }.into());
            }
            if receive {
                let config: MessageCatchConfig = message_configuration(element, marker)?;
                ProcessNodeKind::ReceiveTask {
                    message_ref,
                    correlation_expression: config.correlation_expression,
                    output_mapping: config.output_mapping,
                }
            } else {
                let config: MessageThrowConfig = message_configuration(element, marker)?;
                ProcessNodeKind::SendTask {
                    message_ref,
                    target: config.target,
                    correlation_expression: config.correlation_expression,
                    payload_expression: config.payload_expression,
                    ttl_seconds: config.ttl_seconds,
                }
            }
        }
        "scriptTask" => {
            element.attrs_only(&["id", "name", "scriptFormat"])?;
            ensure!(element.attr("scriptFormat") == Some("application/vnd.tentaflow.cel"),
                "script task {} requires application/vnd.tentaflow.cel at byte {}", id, element.offset);
            for child in &element.children {
                if !child.is(BPMN, "extensionElements") && !child.is(BPMN, "script") {
                    return Err(XmlElementError {
                        message: format!("unsupported script task child at byte {}", child.offset),
                        element_id: Some(id.clone()), offset: child.offset,
                    }.into());
                }
            }
            ensure!(element.text.trim().is_empty(), "script task {} has text outside its body", id);
            let body = element.child(BPMN, "script")?
                .with_context(|| format!("script task {} requires a script body", id))?;
            if element.children.last().is_none_or(|last| !last.is(BPMN, "script")) {
                let offending = element.children.last().expect("script body exists");
                return Err(XmlElementError {
                    message: format!("script task {} has invalid child order at byte {}", id, offending.offset),
                    element_id: Some(id.clone()), offset: offending.offset,
                }.into());
            }
            body.attrs_only(&[])?;
            if let Some(child) = body.children.first() {
                return Err(XmlElementError {
                    message: format!("script task {} body must contain only text at byte {}", id, child.offset),
                    element_id: Some(id.clone()), offset: child.offset,
                }.into());
            }
            ensure!(!body.text.trim().is_empty(), "script task {} requires a nonempty body", id);
            let output_mapping = if let Some(ext) = element.child(BPMN, "extensionElements")? {
                ext.attrs_only(&[])?;
                for child in &ext.children {
                    if !child.is(TF, "scriptTask") {
                        return Err(XmlElementError { message: format!("unsupported script extension at byte {}", child.offset),
                            element_id: Some(id.clone()), offset: child.offset }.into());
                    }
                }
                if let Some(duplicate) = ext.children.get(1) {
                    return Err(XmlElementError { message: format!("duplicate script extension at byte {}", duplicate.offset),
                        element_id: Some(id.clone()), offset: duplicate.offset }.into());
                }
                ensure!(ext.children.len() == 1, "script task {} needs one TentaFlow extension", id);
                let config = ext.child(TF, "scriptTask")?.expect("validated script extension");
                config.attrs_only(&[])?;
                for child in &config.children {
                    if !child.is(TF, "outputMapping") {
                        return Err(XmlElementError { message: format!("unsupported script mapping at byte {}", child.offset),
                            element_id: Some(id.clone()), offset: child.offset }.into());
                    }
                }
                if let Some(duplicate) = config.children.get(1) {
                    return Err(XmlElementError { message: format!("duplicate script mapping at byte {}", duplicate.offset),
                        element_id: Some(id.clone()), offset: duplicate.offset }.into());
                }
                ensure!(config.children.len() == 1, "script task {} needs one output mapping", id);
                let mapping_element = config.child(TF, "outputMapping")?.expect("validated script mapping");
                mapping(config, "outputMapping").map_err(|error| {
                    if error.downcast_ref::<XmlElementError>().is_some() { error }
                    else { XmlElementError { message: format!("invalid script mapping at byte {}: {error}", mapping_element.offset),
                        element_id: Some(id.clone()), offset: mapping_element.offset }.into() }
                })?
            } else {
                BTreeMap::new()
            };
            ProcessNodeKind::ScriptTask { script: body.text.clone(), output_mapping }
        }
        "serviceTask" => {
            element.attrs_only(&["id", "name"])?;
            element.children_only(&[(BPMN, "extensionElements"), (BPMN, "ioSpecification"),
                (BPMN, "dataInputAssociation"), (BPMN, "multiInstanceLoopCharacteristics"),
                (BPMN, "standardLoopCharacteristics")])?;
            let ext = element
                .child(BPMN, "extensionElements")?
                .context("service task requires TentaFlow extension")?;
            ext.attrs_only(&[])?;
            ext.children_only(&[(TF, "service"), (TF, "repeat")])?;
            ensure!(
                ext.children.len() == 1 + usize::from(ext.child(TF, "repeat")?.is_some()),
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
    let repeat = if matches!(kind, ProcessNodeKind::UserTask { .. } | ProcessNodeKind::ServiceTask { .. }) {
        repeat_from_xml(element, target_namespace).map_err(|error| {
            if error.downcast_ref::<XmlElementError>().is_some() { return error; }
            let message = error.to_string();
            let mentioned_offset = message.rsplit_once("at byte ")
                .and_then(|(_, suffix)| suffix.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<usize>().ok());
            let contains_offset = |offset: usize| {
                fn visit(element: &Element, offset: usize) -> bool {
                    element.offset == offset || element.children.iter().any(|child| visit(child, offset))
                }
                visit(element, offset)
            };
            let offset = mentioned_offset.filter(|offset| contains_offset(*offset)).unwrap_or(element.offset);
            XmlElementError { message, element_id: Some(id.clone()), offset }.into()
        })?
    } else { None };
    Ok(ProcessNode { repeat, id, name, kind })
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
    let mut boundary_references = Vec::new();
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
            let node = node_from_xml(child, namespace).map_err(|error| {
                if error.downcast_ref::<XmlElementError>().is_some() { error }
                else { XmlElementError {
                    message: format!("invalid BPMN element {} at byte {}: {error}", child.local, child.offset),
                    element_id: child.attr("id").map(str::to_string), offset: child.offset,
                }.into() }
            })?;
            if let ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
            | ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
            | ProcessNodeKind::BoundaryError { attached_to_id, .. }
            | ProcessNodeKind::BoundaryEscalation { attached_to_id, .. } = &node.kind {
                boundary_references.push((node.id.clone(), attached_to_id.clone(), child.offset));
            }
            nodes.push(node);
        }
    }
    let node_ids: HashSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    for (boundary_id, attached_to_id, offset) in boundary_references {
        if !node_ids.contains(attached_to_id.as_str()) {
            return Err(XmlElementError {
                message: format!("boundary event {boundary_id} references unknown attachment {attached_to_id} at byte {offset}"),
                element_id: Some(boundary_id), offset,
            }.into());
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
    root.children_only(&[
        (BPMN, "process"),
        (BPMNDI, "BPMNDiagram"),
        (BPMN, "message"),
        (BPMN, "error"),
        (BPMN, "escalation"),
        (BPMN, "signal"),
    ])?;
    let has_declarations = root.children.iter().any(|child| {
        child.is(BPMN, "message") || child.is(BPMN, "error")
            || child.is(BPMN, "escalation") || child.is(BPMN, "signal")
    });
    if has_declarations {
        if root.attr("targetNamespace").is_none_or(str::is_empty) {
            return Err(XmlElementError {
                message: "BPMN declarations require explicit targetNamespace".into(),
                element_id: root.attr("id").map(str::to_string),
                offset: root.offset,
            }.into());
        }
    }
    let namespace = root.attr("targetNamespace").unwrap_or(TF);
    let mut messages = Vec::new();
    let mut errors = Vec::new();
    let mut escalations = Vec::new();
    let mut signals = Vec::new();
    for child in &root.children {
        if child.is(BPMN, "message") {
            child.attrs_only(&["id", "name"])?;
            ensure!(
                child.children.is_empty() && child.text.trim().is_empty(),
                "message declaration must be empty"
            );
            messages.push(ProcessMessageDeclaration {
                message_id: child.required("id")?,
                name: child.required("name")?,
            });
        } else if child.is(BPMN, "error") {
            child.attrs_only(&["id", "name", "errorCode"])?;
            ensure!(
                child.children.is_empty() && child.text.trim().is_empty(),
                "error declaration must be empty"
            );
            errors.push(ProcessErrorDeclaration {
                error_id: child.required("id")?,
                name: child.required("name")?,
                error_code: child.required("errorCode")?,
            });
        } else if child.is(BPMN, "escalation") {
            child.attrs_only(&["id", "name", "escalationCode"])?;
            ensure!(
                child.children.is_empty() && child.text.trim().is_empty(),
                "escalation declaration must be empty"
            );
            escalations.push(ProcessEscalationDeclaration {
                escalation_id: child.required("id")?,
                name: child.required("name")?,
                escalation_code: child.required("escalationCode")?,
            });
        } else if child.is(BPMN, "signal") {
            child.attrs_only(&["id", "name"]).map_err(|error| XmlElementError {
                message: error.to_string(), element_id: child.attr("id").map(str::to_string),
                offset: child.offset,
            })?;
            if !child.children.is_empty() || !child.text.trim().is_empty() {
                let offending = child.children.first().unwrap_or(child);
                return Err(XmlElementError { message: format!("signal declaration must be empty at byte {}", offending.offset),
                    element_id: child.attr("id").map(str::to_string), offset: offending.offset }.into());
            }
            signals.push(ProcessSignalDeclaration { signal_id: child.required("id")?,
                namespace_uri: namespace.to_string(), name: child.required("name")? });
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
        let escalation_ref = match &node.kind {
            ProcessNodeKind::BoundaryEscalation { escalation_ref, .. } => escalation_ref.as_deref(),
            _ => None,
        };
        let signal_ref = match &node.kind {
            ProcessNodeKind::SignalCatch { signal_ref, .. }
            | ProcessNodeKind::SignalThrow { signal_ref, .. } => Some(signal_ref.as_str()),
            _ => None,
        };
        if message_ref.is_some_and(|reference| {
            !messages
                .iter()
                .any(|declaration| declaration.message_id == reference)
        }) || error_ref.is_some_and(|reference| {
            !errors
                .iter()
                .any(|declaration| declaration.error_id == reference)
        }) || escalation_ref.is_some_and(|reference| {
            !escalations
                .iter()
                .any(|declaration| declaration.escalation_id == reference)
        }) || signal_ref.is_some_and(|reference| {
            !signals.iter().any(|declaration| declaration.signal_id == reference)
        }) {
            let offset = process
                .children
                .iter()
                .find(|child| child.attr("id") == Some(node.id.as_str()))
                .map_or(process.offset, |child| child.offset);
            return Err(XmlElementError {
                message: format!(
                    "event {} references an unknown or wrong-type declaration at byte {offset}",
                    node.id
                ),
                element_id: Some(node.id.clone()),
                offset,
            }
            .into());
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
        target_namespace: (namespace != TF || !signals.is_empty()).then(|| namespace.to_string()),
        escalations,
        signals,
    };
    validate_model(&model).map_err(|error| {
        if let Some(profile) = error.downcast_ref::<EventGatewayProfileError>() {
            let offset = root.find_id(&profile.node_id)
                .map_or(process.offset, |element| element.offset);
            let context = XmlElementError {
                message: profile.reason.clone(),
                element_id: Some(profile.node_id.clone()),
                offset,
            };
            error.context(context)
        } else if let Some(path) = error.downcast_ref::<EscalationPathError>() {
            let element_id = path
                .flow_id
                .as_ref()
                .or(path.node_id.as_ref())
                .unwrap_or(&path.boundary_id);
            let offset = root
                .find_id(element_id)
                .map_or(process.offset, |element| element.offset);
            let context = XmlElementError {
                message: error.to_string(),
                element_id: Some(element_id.clone()),
                offset,
            };
            error.context(context)
        } else {
            XmlElementError {
                message: error.to_string(),
                element_id: Some(model.process_id.clone()),
                offset: process.offset,
            }
            .into()
        }
    })?;
    Ok(model)
}

pub fn import_xml(xml: &str) -> (Option<ProcessModel>, Vec<ProcessDiagnostic>) {
    match parse_model(xml) {
        Ok(model) => (Some(model), Vec::new()),
        Err(error) => {
            let element = error.downcast_ref::<XmlElementError>();
            let path = error.downcast_ref::<EscalationPathError>();
            (
                None,
                vec![ProcessDiagnostic {
                    code: if path.is_some() {
                        "ESCALATION_IMMEDIATE_PATH_UNSUPPORTED"
                    } else {
                        "UNSUPPORTED_OR_INVALID_BPMN"
                    }
                    .into(),
                    message: error.to_string(),
                    element_id: element.and_then(|element| element.element_id.clone()),
                    offset: element.map(|element| element.offset),
                    fatal: true,
                    boundary_id: path.map(|path| path.boundary_id.clone()),
                    flow_id: path.and_then(|path| path.flow_id.clone()),
                    node_id: path.and_then(|path| path.node_id.clone()),
                    reason: path.map(|path| path.reason),
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

fn write_repeat_xml(xml: &mut String, node: &ProcessNode, used_ids: &mut HashSet<String>) -> Result<()> {
    let Some(repeat) = &node.repeat else { return Ok(()); };
    match repeat {
        ProcessRepeatSpec::MultiInstance { mode, input, .. } => {
            let sequential = matches!(mode, ProcessMultiInstanceMode::Sequential);
            if let ProcessMultiInstanceInput::CollectionExpression { expression } = input {
                let data_id = generated_xml_id(&format!("{}_Collection", node.id), used_ids);
                let item_id = generated_xml_id(&format!("{}_Item", node.id), used_ids);
                xml.push_str(&format!("<bpmn:ioSpecification><bpmn:dataInput id=\"{}\" isCollection=\"true\"/><bpmn:inputSet><bpmn:dataInputRefs>{}</bpmn:dataInputRefs></bpmn:inputSet><bpmn:outputSet/></bpmn:ioSpecification>",
                    escaped(&data_id), escaped(&data_id)));
                xml.push_str(&format!("<bpmn:dataInputAssociation><bpmn:targetRef>{}</bpmn:targetRef><bpmn:assignment><bpmn:from xsi:type=\"bpmn:tFormalExpression\" language=\"https://cel.dev/spec\">{}</bpmn:from><bpmn:to>{}</bpmn:to></bpmn:assignment></bpmn:dataInputAssociation>",
                    escaped(&data_id), escaped(expression), escaped(&data_id)));
                xml.push_str(&format!("<bpmn:multiInstanceLoopCharacteristics isSequential=\"{sequential}\"><bpmn:loopDataInputRef>tns:{}</bpmn:loopDataInputRef><bpmn:inputDataItem id=\"{}\"/></bpmn:multiInstanceLoopCharacteristics>",
                    escaped(&data_id), escaped(&item_id)));
            } else if let ProcessMultiInstanceInput::Cardinality { count } = input {
                xml.push_str(&format!("<bpmn:multiInstanceLoopCharacteristics isSequential=\"{sequential}\"><bpmn:loopCardinality xsi:type=\"bpmn:tFormalExpression\" language=\"https://cel.dev/spec\">{count}</bpmn:loopCardinality></bpmn:multiInstanceLoopCharacteristics>"));
            }
        }
        ProcessRepeatSpec::StructuredLoop { condition, test_before, max_iterations, .. } => {
            xml.push_str(&format!("<bpmn:standardLoopCharacteristics testBefore=\"{test_before}\" loopMaximum=\"{max_iterations}\"><bpmn:loopCondition xsi:type=\"bpmn:tFormalExpression\" language=\"https://cel.dev/spec\">{}</bpmn:loopCondition></bpmn:standardLoopCharacteristics>", escaped(condition)));
        }
    }
    Ok(())
}

fn write_graph(xml: &mut String, nodes: &[ProcessNode], flows: &[ProcessSequenceFlow],
    call_prefixes: &BTreeMap<String, String>, used_ids: &mut HashSet<String>) -> Result<()> {
    for node in nodes {
        let (tag, extra) = match &node.kind {
            ProcessNodeKind::Start => ("startEvent", String::new()),
            ProcessNodeKind::TimerStart { .. } => ("startEvent", String::new()),
            ProcessNodeKind::MessageStart { .. } => ("startEvent", String::new()),
            ProcessNodeKind::TimerCatch { .. } => ("intermediateCatchEvent", String::new()),
            ProcessNodeKind::MessageCatch { .. } => ("intermediateCatchEvent", String::new()),
            ProcessNodeKind::SignalCatch { .. } => ("intermediateCatchEvent", String::new()),
            ProcessNodeKind::MessageThrow { .. } => ("intermediateThrowEvent", String::new()),
            ProcessNodeKind::SignalThrow { .. } => ("intermediateThrowEvent", String::new()),
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
            ProcessNodeKind::BoundaryEscalation { attached_to_id, cancel_activity, .. } => (
                "boundaryEvent",
                format!(" attachedToRef=\"{}\" cancelActivity=\"{}\"", escaped(attached_to_id), cancel_activity),
            ),
            ProcessNodeKind::End => ("endEvent", String::new()),
            ProcessNodeKind::ErrorEnd { .. } => ("endEvent", String::new()),
            ProcessNodeKind::TerminateEnd => ("endEvent", String::new()),
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
            ProcessNodeKind::InclusiveGateway { default_flow_id } => (
                "inclusiveGateway",
                default_flow_id
                    .as_ref()
                    .map(|id| format!(" default=\"{}\"", escaped(id)))
                    .unwrap_or_default(),
            ),
            ProcessNodeKind::UserTask { .. } => ("userTask", String::new()),
            ProcessNodeKind::ServiceTask { .. } => ("serviceTask", String::new()),
            ProcessNodeKind::ScriptTask { .. } => ("scriptTask", " scriptFormat=\"application/vnd.tentaflow.cel\"".into()),
            ProcessNodeKind::ManualTask { .. } => ("manualTask", String::new()),
            ProcessNodeKind::SendTask { message_ref, .. } => ("sendTask",
                format!(" messageRef=\"tns:{}\" implementation=\"##unspecified\"", escaped(message_ref))),
            ProcessNodeKind::ReceiveTask { message_ref, .. } => ("receiveTask",
                format!(" messageRef=\"tns:{}\" implementation=\"##unspecified\" instantiate=\"false\"", escaped(message_ref))),
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
                xml.push_str(&format!("><bpmn:extensionElements><tentaflow:message>{}</tentaflow:message></bpmn:extensionElements><bpmn:messageEventDefinition messageRef=\"tns:{}\"/></bpmn:{tag}>",
                    escaped(&serde_json::to_string(&config)?), escaped(message_ref)));
            }
            ProcessNodeKind::MessageCatch { message_ref, correlation_expression, output_mapping }
            | ProcessNodeKind::BoundaryMessage { message_ref, correlation_expression, output_mapping, .. } => {
                let config = serde_json::json!({ "correlation_expression": correlation_expression, "output_mapping": output_mapping });
                xml.push_str(&format!("><bpmn:extensionElements><tentaflow:message>{}</tentaflow:message></bpmn:extensionElements><bpmn:messageEventDefinition messageRef=\"tns:{}\"/></bpmn:{tag}>",
                    escaped(&serde_json::to_string(&config)?), escaped(message_ref)));
            }
            ProcessNodeKind::MessageThrow { message_ref, target, correlation_expression, payload_expression, ttl_seconds } => {
                let config = serde_json::json!({ "target": target, "correlation_expression": correlation_expression,
                    "payload_expression": payload_expression, "ttl_seconds": ttl_seconds });
                xml.push_str(&format!("><bpmn:extensionElements><tentaflow:message>{}</tentaflow:message></bpmn:extensionElements><bpmn:messageEventDefinition messageRef=\"tns:{}\"/></bpmn:{tag}>",
                    escaped(&serde_json::to_string(&config)?), escaped(message_ref)));
            }
            ProcessNodeKind::SignalCatch { signal_ref, output_mapping } => {
                let config = serde_json::json!({ "output_mapping": output_mapping });
                xml.push_str(&format!("><bpmn:extensionElements><tentaflow:signalCatch>{}</tentaflow:signalCatch></bpmn:extensionElements><bpmn:signalEventDefinition signalRef=\"tns:{}\"/></bpmn:{tag}>",
                    escaped(&serde_json::to_string(&config)?), escaped(signal_ref)));
            }
            ProcessNodeKind::SignalThrow { signal_ref, payload_expression, ttl_seconds } => {
                let config = serde_json::json!({ "payload_expression": payload_expression, "ttl_seconds": ttl_seconds });
                xml.push_str(&format!("><bpmn:extensionElements><tentaflow:signalThrow>{}</tentaflow:signalThrow></bpmn:extensionElements><bpmn:signalEventDefinition signalRef=\"tns:{}\"/></bpmn:{tag}>",
                    escaped(&serde_json::to_string(&config)?), escaped(signal_ref)));
            }
            ProcessNodeKind::BoundaryError {
                error_ref,
                output_mapping,
                ..
            } => {
                let reference = error_ref
                    .as_ref()
                    .map(|id| format!(" errorRef=\"tns:{}\"", escaped(id)))
                    .unwrap_or_default();
                xml.push('>');
                if !output_mapping.is_empty() {
                    xml.push_str(&format!("<bpmn:extensionElements><tentaflow:outputMapping>{}</tentaflow:outputMapping></bpmn:extensionElements>",
                        escaped(&serde_json::to_string(output_mapping)?)));
                }
                xml.push_str(&format!("<bpmn:errorEventDefinition{reference}/>"));
                xml.push_str(&format!("</bpmn:{tag}>"));
            }
            ProcessNodeKind::BoundaryEscalation {
                escalation_ref,
                output_mapping,
                ..
            } => {
                let reference = escalation_ref
                    .as_ref()
                    .map(|id| format!(" escalationRef=\"tns:{}\"", escaped(id)))
                    .unwrap_or_default();
                xml.push('>');
                if !output_mapping.is_empty() {
                    xml.push_str(&format!("<bpmn:extensionElements><tentaflow:outputMapping>{}</tentaflow:outputMapping></bpmn:extensionElements>",
                        escaped(&serde_json::to_string(output_mapping)?)));
                }
                xml.push_str(&format!("<bpmn:escalationEventDefinition{reference}/>"));
                xml.push_str(&format!("</bpmn:{tag}>"));
            }
            ProcessNodeKind::ErrorEnd { error_ref } => {
                xml.push_str(&format!("><bpmn:errorEventDefinition errorRef=\"tns:{}\"/></bpmn:{tag}>",
                    escaped(error_ref)));
            }
            ProcessNodeKind::TerminateEnd => {
                xml.push_str(&format!("><bpmn:terminateEventDefinition/></bpmn:{tag}>"));
            }
            ProcessNodeKind::UserTask {
                assignee_user_id,
                output_mapping,
            } => {
                if assignee_user_id.is_none() && output_mapping.is_empty() && node.repeat.is_none() {
                    xml.push_str("/>");
                } else {
                    xml.push_str("><bpmn:extensionElements>");
                    if assignee_user_id.is_some() || !output_mapping.is_empty() {
                        xml.push_str("<tentaflow:user");
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
                        xml.push_str("</tentaflow:user>");
                    }
                    if let Some(repeat) = &node.repeat {
                        let output = match repeat {
                            ProcessRepeatSpec::MultiInstance { output_collection_variable, .. }
                            | ProcessRepeatSpec::StructuredLoop { output_collection_variable, .. } => output_collection_variable,
                        };
                        xml.push_str(&format!("<tentaflow:repeat outputCollectionVariable=\"{}\"/>", escaped(output)));
                    }
                    xml.push_str("</bpmn:extensionElements>");
                    write_repeat_xml(xml, node, used_ids)?;
                    xml.push_str(&format!("</bpmn:{tag}>"));
                }
            }
            ProcessNodeKind::ScriptTask { script, output_mapping } => {
                xml.push('>');
                if !output_mapping.is_empty() {
                    xml.push_str(&format!("<bpmn:extensionElements><tentaflow:scriptTask><tentaflow:outputMapping>{}</tentaflow:outputMapping></tentaflow:scriptTask></bpmn:extensionElements>",
                        escaped(&serde_json::to_string(output_mapping)?)));
                }
                xml.push_str(&format!("<bpmn:script>{}</bpmn:script></bpmn:{tag}>", escaped(script)));
            }
            ProcessNodeKind::ManualTask { assignee_user_id, instructions } => {
                xml.push('>');
                if !instructions.is_empty() {
                    xml.push_str(&format!("<bpmn:documentation textFormat=\"text/plain\">{}</bpmn:documentation>", escaped(instructions)));
                }
                xml.push_str("<bpmn:extensionElements><tentaflow:manual");
                if let Some(assignee) = assignee_user_id {
                    xml.push_str(&format!(" assigneeUserId=\"{}\"", escaped(assignee)));
                }
                xml.push_str("/></bpmn:extensionElements></bpmn:manualTask>");
            }
            ProcessNodeKind::SendTask { target, correlation_expression, payload_expression, ttl_seconds, .. } => {
                let config = serde_json::json!({ "target": target, "correlation_expression": correlation_expression,
                    "payload_expression": payload_expression, "ttl_seconds": ttl_seconds });
                xml.push_str(&format!("><bpmn:extensionElements><tentaflow:sendTask>{}</tentaflow:sendTask></bpmn:extensionElements></bpmn:sendTask>",
                    escaped(&serde_json::to_string(&config)?)));
            }
            ProcessNodeKind::ReceiveTask { correlation_expression, output_mapping, .. } => {
                let config = serde_json::json!({ "correlation_expression": correlation_expression,
                    "output_mapping": output_mapping });
                xml.push_str(&format!("><bpmn:extensionElements><tentaflow:receiveTask>{}</tentaflow:receiveTask></bpmn:extensionElements></bpmn:receiveTask>",
                    escaped(&serde_json::to_string(&config)?)));
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
                xml.push_str("</tentaflow:service>");
                if let Some(repeat) = &node.repeat {
                    let output = match repeat {
                        ProcessRepeatSpec::MultiInstance { output_collection_variable, .. }
                        | ProcessRepeatSpec::StructuredLoop { output_collection_variable, .. } => output_collection_variable,
                    };
                    xml.push_str(&format!("<tentaflow:repeat outputCollectionVariable=\"{}\"/>", escaped(output)));
                }
                xml.push_str("</bpmn:extensionElements>");
                write_repeat_xml(xml, node, used_ids)?;
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
                write_graph(xml, &body.nodes, &body.sequence_flows, call_prefixes, used_ids)?;
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
    let has_repeat = super::model::all_nodes(model).iter().any(|node| node.repeat.is_some());
    let declarations =
        !model.messages.is_empty() || !model.errors.is_empty() || !model.escalations.is_empty()
            || !model.signals.is_empty();
    let call_namespaces: BTreeSet<_> = super::model::all_nodes(model)
        .into_iter()
        .filter_map(|node| {
            if let ProcessNodeKind::CallActivity { called_element, .. } = &node.kind {
                Some(called_element.namespace_uri.clone())
            } else {
                None
            }
        })
        .collect();
    let mut call_prefixes = BTreeMap::new();
    let tns = if declarations || has_repeat || call_namespaces.contains(namespace) {
        call_prefixes.insert(namespace.to_string(), "tns".to_string());
        format!(" xmlns:tns=\"{}\"", escaped(namespace))
    } else { String::new() };
    let mut call_namespaces_xml = String::new();
    for (index, uri) in call_namespaces.into_iter().filter(|uri| uri.as_str() != namespace).enumerate() {
        let prefix = format!("call{}", index + 1);
        call_namespaces_xml.push_str(&format!(" xmlns:{prefix}=\"{}\"", escaped(&uri)));
        call_prefixes.insert(uri, prefix);
    }
    let xsi = if has_repeat { format!(" xmlns:xsi=\"{XSI}\"") } else { String::new() };
    let mut xml=format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><bpmn:definitions xmlns:bpmn=\"{BPMN}\" xmlns:bpmndi=\"{BPMNDI}\" xmlns:dc=\"{DC}\" xmlns:di=\"{DI}\" xmlns:tentaflow=\"{TF}\"{tns}{call_namespaces_xml}{xsi} targetNamespace=\"{}\">", escaped(namespace));
    for message in &model.messages {
        xml.push_str(&format!("<bpmn:message id=\"{}\" name=\"{}\"/>", escaped(&message.message_id), escaped(&message.name)));
    }
    for error in &model.errors {
        xml.push_str(&format!(
            "<bpmn:error id=\"{}\" name=\"{}\" errorCode=\"{}\"/>",
            escaped(&error.error_id),
            escaped(&error.name),
            escaped(&error.error_code)
        ));
    }
    for escalation in &model.escalations {
        xml.push_str(&format!(
            "<bpmn:escalation id=\"{}\" name=\"{}\" escalationCode=\"{}\"/>",
            escaped(&escalation.escalation_id),
            escaped(&escalation.name),
            escaped(&escalation.escalation_code)
        ));
    }
    for signal in &model.signals {
        xml.push_str(&format!("<bpmn:signal id=\"{}\" name=\"{}\"/>",
            escaped(&signal.signal_id), escaped(&signal.name)));
    }
    xml.push_str(&format!(
        "<bpmn:process id=\"{}\" isExecutable=\"true\">",
        escaped(&model.process_id)
    ));
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
    let mut used_ids = HashSet::from([model.process_id.clone()]);
    used_ids.extend(
        model
            .messages
            .iter()
            .map(|declaration| declaration.message_id.clone()),
    );
    used_ids.extend(
        model
            .errors
            .iter()
            .map(|declaration| declaration.error_id.clone()),
    );
    used_ids.extend(
        model
            .escalations
            .iter()
            .map(|declaration| declaration.escalation_id.clone()),
    );
    used_ids.extend(model.signals.iter().map(|declaration| declaration.signal_id.clone()));
    used_ids.extend(model.sequence_flows.iter().map(|flow| flow.id.clone()));
    let mut shapes = Vec::new();
    let mut edges = Vec::new();
    collect_diagram(
        &model.nodes,
        &model.diagram,
        &mut shapes,
        &mut edges,
        &mut used_ids,
    );
    write_graph(&mut xml, &model.nodes, &model.sequence_flows, &call_prefixes, &mut used_ids)?;
    xml.push_str("</bpmn:process>");
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
        model.nodes.insert(1, ProcessNode { repeat: None,
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
        model.variables.insert("review_result".into(), serde_json::Value::Null);
        model.messages = vec![
            ProcessMessageDeclaration { message_id: "Message_Start".into(), name: "order.received".into() },
            ProcessMessageDeclaration { message_id: "Message_Throw".into(), name: "order.sent".into() },
        ];
        model.errors = vec![ProcessErrorDeclaration { error_id: "Error_Validation".into(), name: "Bad & <order>".into(), error_code: "BUSINESS.INVALID".into() }];
        model.nodes[0].kind = ProcessNodeKind::MessageStart { message_ref: "Message_Start".into(),
            output_mapping: BTreeMap::from([("customer_ID".into(), "outputs.customer_ID".into())]) };
        model.nodes.push(ProcessNode { repeat: None, id: "Service_1".into(), name: "Check".into(), kind: ProcessNodeKind::ServiceTask {
            flow_id: "flow-123".into(), input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
            verification: ActivityVerification::Human, timeout_seconds: 60,
            result_expression: Some("vars.business_result".into()),
        } });
        model.nodes.push(ProcessNode { repeat: None, id: "Throw_1".into(), name: "Send".into(), kind: ProcessNodeKind::MessageThrow {
            message_ref: "Message_Throw".into(), target: ProcessMessageTargetSpec::Catch {
                definition_id: "7c865aaa-febd-4621-9ae6-35977200a0fd".into(),
                instance_id_expression: Some("vars.instance_ID".into()), subscription_id_expression: None,
            }, correlation_expression: "vars.customer_ID".into(), payload_expression: "vars.payload".into(), ttl_seconds: 60,
        } });
        model.nodes.push(ProcessNode { repeat: None, id: "Boundary_Error".into(), name: "Error".into(), kind: ProcessNodeKind::BoundaryError {
            attached_to_id: "Service_1".into(), error_ref: Some("Error_Validation".into()),
            output_mapping: BTreeMap::from([("review_result".into(), "outputs.result".into())]),
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
        for (tag, id, definition) in [
            ("startEvent", model.nodes[0].id.as_str(), "messageEventDefinition"),
            ("intermediateThrowEvent", "Throw_1", "messageEventDefinition"),
            ("boundaryEvent", "Boundary_Error", "errorEventDefinition"),
        ] {
            let opening = format!("<bpmn:{tag} id=\"{id}\"");
            let start = xml.find(&opening).unwrap();
            let end = start + xml[start..].find(&format!("</bpmn:{tag}>")).unwrap();
            let event = &xml[start..end];
            assert!(event.find("<bpmn:extensionElements>").unwrap()
                < event.find(&format!("<bpmn:{definition}")).unwrap(), "{id} has invalid BPMN element order");
        }
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
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.fatal && diagnostic.offset.is_some()),
                "{diagnostics:?}"
            );
        }
    }

    #[test]
    fn message_catch_extension_precedes_event_definition_and_preserves_di() {
        let mut model = super::super::model::starter_model();
        model.messages.push(ProcessMessageDeclaration {
            message_id: "Message_1".into(), name: "order.received".into(),
        });
        model.variables.insert("case_key".into(), serde_json::json!("case-1"));
        model.nodes.insert(1, ProcessNode { id: "Catch_1".into(), name: "Wait <&>".into(), repeat: None,
            kind: ProcessNodeKind::MessageCatch { message_ref: "Message_1".into(),
                correlation_expression: "vars.case_key".into(), output_mapping: BTreeMap::new() } });
        model.sequence_flows[0].target_id = "Catch_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_Catch".into(),
            source_id: "Catch_1".into(), target_id: "End_1".into(), condition: None });
        model.diagram.shapes.push(ProcessShape { element_id: "Catch_1".into(),
            x: 180.0, y: 100.0, width: 56.0, height: 56.0 });
        let xml = export_xml(&model).unwrap();
        let start = xml.find("<bpmn:intermediateCatchEvent id=\"Catch_1\"").unwrap();
        let end = start + xml[start..].find("</bpmn:intermediateCatchEvent>").unwrap();
        let event = &xml[start..end];
        assert!(event.find("<bpmn:extensionElements>").unwrap()
            < event.find("<bpmn:messageEventDefinition").unwrap());
        assert!(xml.contains("bpmnElement=\"Catch_1\""));
        assert_eq!(import_xml(&xml).0, Some(model));
    }

    #[test]
    fn escalation_xml_round_trip_and_immediate_path_diagnostic_preserve_context() {
        let mut model = super::super::model::starter_model();
        model.target_namespace = Some("urn:example:review".into());
        model.escalations.push(ProcessEscalationDeclaration {
            escalation_id: "Escalation_1".into(),
            name: "Review & approve".into(),
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
                        "business_key".into(),
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
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<bpmn:escalation id=\"Escalation_1\""));
        assert!(xml.contains("escalationRef=\"tns:Escalation_1\""));
        let start = xml.find("<bpmn:boundaryEvent id=\"Boundary_1\"").unwrap();
        let end = start + xml[start..].find("</bpmn:boundaryEvent>").unwrap();
        let boundary = &xml[start..end];
        assert!(boundary.find("<bpmn:extensionElements>").unwrap()
            < boundary.find("<bpmn:escalationEventDefinition").unwrap());
        assert_eq!(import_xml(&xml).0, Some(model));
        let wrong_type = xml.replace(
            "escalationRef=\"tns:Escalation_1\"",
            "escalationRef=\"tns:Unknown_1\"",
        );
        let (restored, diagnostics) = import_xml(&wrong_type);
        assert!(restored.is_none());
        assert_eq!(diagnostics[0].element_id.as_deref(), Some("Boundary_1"));
        let immediate_end = xml.replace("targetRef=\"Wait_1\"", "targetRef=\"End_1\"");
        let (restored, diagnostics) = import_xml(&immediate_end);
        assert!(restored.is_none());
        assert_eq!(diagnostics[0].code, "ESCALATION_IMMEDIATE_PATH_UNSUPPORTED");
        assert_eq!(diagnostics[0].boundary_id.as_deref(), Some("Boundary_1"));
        assert_eq!(diagnostics[0].flow_id.as_deref(), Some("Flow_Escalation"));
        assert_eq!(
            diagnostics[0].element_id.as_deref(),
            Some("Flow_Escalation")
        );
        assert!(diagnostics[0].offset.is_some_and(|offset| offset > 0));
        assert_eq!(
            diagnostics[0].reason,
            Some(tentaflow_protocol::processes::ProcessEscalationPathReason::TerminalBeforeWait)
        );
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
            ProcessNode { repeat: None, id: "Race_1".into(), name: "First arrival".into(), kind: ProcessNodeKind::EventBasedGateway },
            ProcessNode { repeat: None, id: "Catch_A".into(), name: "First".into(), kind: ProcessNodeKind::MessageCatch {
                message_ref: "Message_1".into(), correlation_expression: "vars.case_ID".into(), output_mapping: BTreeMap::new(),
            } },
            ProcessNode { repeat: None, id: "Catch_B".into(), name: "Second".into(), kind: ProcessNodeKind::MessageCatch {
                message_ref: "Message_1".into(), correlation_expression: "vars.case_ID".into(), output_mapping: BTreeMap::new(),
            } },
            ProcessNode { repeat: None, id: "Service_1".into(), name: "Check".into(), kind: ProcessNodeKind::ServiceTask {
                flow_id: "flow-123".into(), input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
                verification: ActivityVerification::Human, timeout_seconds: 60, result_expression: None,
            } },
            ProcessNode { repeat: None, id: "Boundary_1".into(), name: "Reminder".into(), kind: ProcessNodeKind::BoundaryMessage {
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
        model.nodes.push(ProcessNode { repeat: None,
            id: "Review_1".into(), name: "Review & approve".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None, output_mapping: BTreeMap::new(),
            },
        });
        model.nodes.push(ProcessNode { repeat: None,
            id: "Boundary_A".into(), name: "Deadline".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Review_1".into(), cancel_activity: true,
                timer: ProcessTimerSpec::Date { at: "2027-01-02T03:04:05+01:00".into() },
            },
        });
        model.nodes.push(ProcessNode { repeat: None,
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
                model.nodes.push(ProcessNode { repeat: None, id: "Wait_1".into(), name: "Wait & resume".into(), kind: ProcessNodeKind::TimerCatch { timer: rule } });
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
        model.nodes.push(ProcessNode { repeat: None,
            id: "Review_1".into(),
            name: "R&D \"Łódź\"".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            },
        });
        model.nodes.push(ProcessNode { repeat: None,
            id: "Route_1".into(),
            name: "Route".into(),
            kind: ProcessNodeKind::ExclusiveGateway {
                default_flow_id: Some("Flow_4".into()),
            },
        });
        model.nodes.push(ProcessNode { repeat: None,
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
        model.nodes.push(ProcessNode { repeat: None,
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
        model.nodes.insert(1, ProcessNode { repeat: None,
            id: "Sub_1".into(), name: "Review & scope".into(),
            kind: ProcessNodeKind::SubProcess {
                body: ProcessSubProcess {
                    nodes: vec![
                        ProcessNode { repeat: None, id: "LocalStart".into(), name: "Local start".into(), kind: ProcessNodeKind::Start },
                        ProcessNode { repeat: None, id: "LocalEnd".into(), name: "Local end".into(), kind: ProcessNodeKind::End },
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

        model.timer_timezone = Some("UTC".into());
        model.messages.push(ProcessMessageDeclaration {
            message_id: "Message_Local".into(), name: "local.reply".into(),
        });
        if let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes[1].kind {
            body.nodes.insert(1, ProcessNode { repeat: None, id: "LocalSend".into(),
                name: "Zażółć 日本語 — Admit & wait".into(), kind: ProcessNodeKind::SendTask {
                    message_ref: "Message_Local".into(),
                    target: ProcessMessageTargetSpec::Start {
                        definition_id: uuid::Uuid::nil().to_string(),
                    },
                    correlation_expression: "vars.local_ID".into(),
                    payload_expression: "vars.local_ID".into(), ttl_seconds: 60,
                } });
            body.nodes.push(ProcessNode { repeat: None, id: "LocalTimer".into(),
                name: "Deadline".into(), kind: ProcessNodeKind::BoundaryTimer {
                    attached_to_id: "LocalSend".into(), cancel_activity: true,
                    timer: ProcessTimerSpec::Duration { seconds: 60 },
                } });
            body.nodes.push(ProcessNode { repeat: None, id: "LocalMessage".into(),
                name: "Reply".into(), kind: ProcessNodeKind::BoundaryMessage {
                    attached_to_id: "LocalSend".into(), cancel_activity: false,
                    message_ref: "Message_Local".into(),
                    correlation_expression: "vars.local_ID".into(),
                    output_mapping: BTreeMap::new(),
                } });
            body.sequence_flows[0].target_id = "LocalSend".into();
            for (id, source_id) in [("LocalSendFlow", "LocalSend"),
                ("LocalTimerFlow", "LocalTimer"), ("LocalMessageFlow", "LocalMessage")] {
                body.sequence_flows.push(ProcessSequenceFlow {
                    id: id.into(), source_id: source_id.into(), target_id: "LocalEnd".into(),
                    condition: None,
                });
            }
            for (id, x, y, width, height) in [
                ("LocalSend", 130.0, 40.0, 160.0, 96.0),
                ("LocalTimer", 260.0, 115.0, 56.0, 56.0),
                ("LocalMessage", 120.0, 115.0, 56.0, 56.0),
            ] {
                body.diagram.shapes.push(ProcessShape {
                    element_id: id.into(), x, y, width, height,
                });
            }
            for id in ["LocalSendFlow", "LocalTimerFlow", "LocalMessageFlow"] {
                body.diagram.edges.push(ProcessEdgeDiagram {
                    sequence_flow_id: id.into(), waypoints: vec![
                        ProcessPoint { x: 290.0, y: 88.0 },
                        ProcessPoint { x: 360.0, y: 88.0 },
                    ],
                });
            }
        }
        let embedded_xml = export_xml(&model).unwrap();
        assert!(embedded_xml.contains("<bpmn:sendTask id=\"LocalSend\""));
        assert!(embedded_xml.contains("attachedToRef=\"LocalSend\""));
        assert!(embedded_xml.contains("bpmnElement=\"LocalTimer\""));
        assert!(embedded_xml.contains("bpmnElement=\"LocalMessage\""));
        assert_eq!(import_xml(&embedded_xml).0, Some(model));
        for invalid in [
            embedded_xml.replacen(" attachedToRef=\"LocalSend\"", "", 1),
            embedded_xml.replacen("attachedToRef=\"LocalSend\"",
                "attachedToRef=\"MissingLocalActivity\"", 1),
        ] {
            let offset = invalid.find("<bpmn:boundaryEvent id=\"LocalTimer\"").unwrap();
            assert!(invalid[..offset].chars().count() < offset);
            let (restored, diagnostics) = import_xml(&invalid);
            assert!(restored.is_none());
            assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
                && diagnostic.element_id.as_deref() == Some("LocalTimer")
                && diagnostic.offset == Some(offset)), "{diagnostics:?}");
        }
    }

    #[test]
    fn inclusive_gateway_xml_preserves_conditions_default_ids_and_di() {
        let mut model = super::super::model::starter_model();
        model.variables.insert("approval_ID".into(), serde_json::json!("A&B"));
        model.nodes.extend([
            ProcessNode { repeat: None, id: "OR_Split".into(), name: "Select & notify".into(),
                kind: ProcessNodeKind::InclusiveGateway { default_flow_id: Some("Flow_Default".into()) } },
            ProcessNode { repeat: None, id: "Task_A".into(), name: "Review A".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } },
            ProcessNode { repeat: None, id: "Task_B".into(), name: "Review B".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } },
            ProcessNode { repeat: None, id: "OR_Join".into(), name: "All selected".into(),
                kind: ProcessNodeKind::InclusiveGateway { default_flow_id: None } },
        ]);
        model.sequence_flows[0].target_id = "OR_Split".into();
        for (id, source, target, condition) in [
            ("Flow_A", "OR_Split", "Task_A", Some("vars.approval_ID == \"A&B\"")),
            ("Flow_Default", "OR_Split", "Task_B", None),
            ("Flow_A_Join", "Task_A", "OR_Join", None),
            ("Flow_B_Join", "Task_B", "OR_Join", None),
            ("Flow_End", "OR_Join", "End_1", None),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                id: id.into(), source_id: source.into(), target_id: target.into(),
                condition: condition.map(str::to_string),
            });
        }
        model.diagram.shapes.push(ProcessShape {
            element_id: "OR_Split".into(), x: 120.0, y: 160.0, width: 72.0, height: 72.0,
        });
        model.diagram.edges.push(ProcessEdgeDiagram {
            sequence_flow_id: "Flow_A".into(),
            waypoints: vec![ProcessPoint { x: 192.0, y: 196.0 }, ProcessPoint { x: 340.0, y: 196.0 }],
        });
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<bpmn:inclusiveGateway id=\"OR_Split\""));
        assert!(xml.contains("default=\"Flow_Default\""));
        assert!(xml.contains("vars.approval_ID == &quot;A&amp;B&quot;"));
        let (parsed, diagnostics) = import_xml(&xml);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(parsed, Some(model));

        let invalid = xml.replacen("default=\"Flow_Default\"", "default=\"Flow_Missing\"", 1);
        let (parsed, diagnostics) = import_xml(&invalid);
        assert!(parsed.is_none());
        assert!(diagnostics[0].fatal && diagnostics[0].message.contains("OR_Split"));
        assert!(diagnostics[0].offset.is_some());
        let invalid = xml.replacen("<bpmn:inclusiveGateway", "<bpmn:foreignGateway", 1);
        assert!(import_xml(&invalid).0.is_none());
    }

    #[test]
    fn terminate_end_xml_round_trip_and_rejects_mixed_definitions() {
        let mut model = super::super::model::starter_model();
        model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind = ProcessNodeKind::TerminateEnd;
        model.nodes[0].name = "Zażółć — source before the terminal".into();
        model.diagram.shapes.push(ProcessShape {
            element_id: "End_1".into(), x: 260.0, y: 80.0, width: 56.0, height: 56.0,
        });
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<bpmn:endEvent id=\"End_1\""));
        assert!(xml.contains("<bpmn:terminateEventDefinition/>"));
        assert!(xml.contains("bpmnElement=\"End_1\""), "the terminal retains its BPMN DI shape");
        assert_eq!(import_xml(&xml).0, Some(model.clone()));
        let alternate_prefix = xml.replacen(&format!("xmlns:bpmn=\"{BPMN}\""),
            &format!("xmlns:bpmn=\"{BPMN}\" xmlns:alternate=\"{BPMN}\""), 1)
            .replace("<bpmn:terminateEventDefinition/>", "<alternate:terminateEventDefinition/>");
        assert_eq!(import_xml(&alternate_prefix).0, Some(model.clone()));

        for (label, invalid, offending_child) in [
            ("unsupported id", xml.replacen("<bpmn:terminateEventDefinition/>",
                "<bpmn:terminateEventDefinition id=\"UnsupportedDefinition\"/>", 1),
                "<bpmn:terminateEventDefinition id=\"UnsupportedDefinition\""),
            ("duplicate", xml.replacen("<bpmn:terminateEventDefinition/>",
                "<bpmn:terminateEventDefinition/><bpmn:terminateEventDefinition/>", 1),
                "<bpmn:terminateEventDefinition/><bpmn:terminateEventDefinition/>"),
            ("mixed", xml.replacen("<bpmn:terminateEventDefinition/>",
                "<bpmn:terminateEventDefinition/><bpmn:errorEventDefinition errorRef=\"tns:Missing\"/>", 1),
                "<bpmn:errorEventDefinition"),
            ("plain global", xml.replacen("<bpmn:terminateEventDefinition/>",
                "<bpmn:terminateEventDefinition terminateAll=\"true\"/>", 1),
                "<bpmn:terminateEventDefinition terminateAll="),
            ("foreign global", xml.replacen(&format!("xmlns:bpmn=\"{BPMN}\""),
                &format!("xmlns:bpmn=\"{BPMN}\" xmlns:camunda=\"http://camunda.org/schema/1.0/bpmn\""), 1)
                .replacen("<bpmn:terminateEventDefinition/>",
                    "<bpmn:terminateEventDefinition camunda:terminateAll=\"true\"/>", 1),
                "<bpmn:terminateEventDefinition camunda:terminateAll="),
            ("foreign namespace", xml.replacen(&format!("xmlns:bpmn=\"{BPMN}\""),
                &format!("xmlns:bpmn=\"{BPMN}\" xmlns:foreign=\"urn:foreign:events\""), 1)
                .replacen("<bpmn:terminateEventDefinition/>", "<foreign:terminateEventDefinition/>", 1),
                "<foreign:terminateEventDefinition"),
            ("nested content", xml.replacen("<bpmn:terminateEventDefinition/>",
                "<bpmn:terminateEventDefinition><bpmn:extensionElements/></bpmn:terminateEventDefinition>", 1),
                "<bpmn:terminateEventDefinition>"),
        ] {
            let (restored, diagnostics) = import_xml(&invalid);
            assert!(restored.is_none(), "{label}");
            let expected_offset = if label == "duplicate" {
                invalid.find(offending_child).unwrap() + "<bpmn:terminateEventDefinition/>".len()
            } else { invalid.find(offending_child).unwrap() };
            assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
                && diagnostic.element_id.as_deref() == Some("End_1")
                && diagnostic.offset == Some(expected_offset)), "{label}: {diagnostics:?}");
            assert!(invalid[..expected_offset].contains("Zażółć"),
                "{label} must measure UTF-8 bytes after a non-ASCII process name");
        }
    }

    #[test]
    fn repetition_xml_round_trips_cardinality_collection_and_structured_loop() {
        let mut model = super::super::model::starter_model();
        model.process_id = "Collection_Review".into();
        model.variables.insert("results".into(), serde_json::json!([]));
        model.variables.insert("items".into(), serde_json::json!([{"case": "A"}]));
        model.nodes.insert(1, ProcessNode { id: "Review".into(), name: "Zażółć review".into(),
            kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() },
            repeat: Some(ProcessRepeatSpec::MultiInstance { mode: ProcessMultiInstanceMode::Parallel,
                input: ProcessMultiInstanceInput::Cardinality { count: 16 },
                output_collection_variable: "results".into() }) });
        model.sequence_flows[0].target_id = "Review".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Next".into(), source_id: "Review".into(), target_id: "End_1".into(), condition: None });
        for repeat in [
            ProcessRepeatSpec::MultiInstance { mode: ProcessMultiInstanceMode::Parallel,
                input: ProcessMultiInstanceInput::Cardinality { count: 16 }, output_collection_variable: "results".into() },
            ProcessRepeatSpec::MultiInstance { mode: ProcessMultiInstanceMode::Sequential,
                input: ProcessMultiInstanceInput::CollectionExpression { expression: "vars.items".into() },
                output_collection_variable: "results".into() },
            ProcessRepeatSpec::StructuredLoop { condition: "vars.keep_going".into(), test_before: true,
                max_iterations: 32, output_collection_variable: "results".into() },
        ] {
            model.nodes[1].repeat = Some(repeat);
            let xml = export_xml(&model).unwrap();
            let (restored, diagnostics) = import_xml(&xml);
            assert!(diagnostics.is_empty(), "{diagnostics:?}");
            assert_eq!(restored.unwrap(), model);
            assert!(xml.contains("outputCollectionVariable=\"results\""));
            if xml.contains("loopDataInputRef") {
                assert!(xml.contains("<bpmn:dataInput ") && xml.contains("isCollection=\"true\""));
                assert!(xml.contains("<bpmn:assignment><bpmn:from xsi:type="));
                assert!(xml.contains("<bpmn:loopDataInputRef>tns:"));
                let encoded_ref = xml.replacen("<bpmn:loopDataInputRef>tns:Review_Collection",
                    "<bpmn:loopDataInputRef>tns:Review_&#67;ollection", 1);
                assert_ne!(encoded_ref, xml);
                assert_eq!(import_xml(&encoded_ref).0, Some(model.clone()));
            }
            if xml.contains("isSequential=\"true\"") {
                let lexical = xml.replacen("isSequential=\"true\"", "isSequential=\"1\"", 1)
                    .replacen("isCollection=\"true\"", "isCollection=\"1\"", 1);
                assert_eq!(import_xml(&lexical).0, Some(model.clone()));
            }
            if xml.contains("testBefore=\"true\"") {
                let lexical = xml.replacen("testBefore=\"true\"", "testBefore=\"1\"", 1);
                assert_eq!(import_xml(&lexical).0, Some(model.clone()));
            }
        }
        model.nodes[1].kind = ProcessNodeKind::ServiceTask {
            flow_id: "flow-review".into(), input_mapping: BTreeMap::new(),
            output_mapping: BTreeMap::new(), verification: ActivityVerification::Human,
            timeout_seconds: 60, result_expression: None,
        };
        model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::CollectionExpression { expression: "vars.items".into() },
            output_collection_variable: "results".into(),
        });
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<bpmn:serviceTask id=\"Review\""));
        assert_eq!(import_xml(&xml).0, Some(model));
    }

    #[test]
    fn repetition_xml_rejects_wrong_qname_and_duplicate_loop_with_utf8_child_offset() {
        let mut model = super::super::model::starter_model();
        model.variables.insert("results".into(), serde_json::json!([]));
        model.variables.insert("items".into(), serde_json::json!([1]));
        model.nodes.insert(1, ProcessNode { id: "Review".into(), name: "Zażółć review".into(),
            kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() },
            repeat: Some(ProcessRepeatSpec::MultiInstance { mode: ProcessMultiInstanceMode::Sequential,
                input: ProcessMultiInstanceInput::CollectionExpression { expression: "vars.items".into() },
                output_collection_variable: "results".into() }) });
        model.sequence_flows[0].target_id = "Review".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Next".into(), source_id: "Review".into(), target_id: "End_1".into(), condition: None });
        let xml = export_xml(&model).unwrap();
        let wrong_qname = xml.replacen("<bpmn:loopDataInputRef>tns:", "<bpmn:loopDataInputRef>bpmn:", 1);
        let duplicate = xml.replacen("</bpmn:multiInstanceLoopCharacteristics>",
            "</bpmn:multiInstanceLoopCharacteristics><bpmn:standardLoopCharacteristics loopMaximum=\"2\" testBefore=\"true\"><bpmn:loopCondition xsi:type=\"bpmn:tFormalExpression\" language=\"https://cel.dev/spec\">true</bpmn:loopCondition></bpmn:standardLoopCharacteristics>", 1);
        for (invalid, marker) in [(wrong_qname, "<bpmn:loopDataInputRef>"),
            (duplicate, "<bpmn:standardLoopCharacteristics")] {
            let (restored, diagnostics) = import_xml(&invalid);
            assert!(restored.is_none());
            let offset = invalid.find(marker).unwrap();
            assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
                && diagnostic.element_id.as_deref() == Some("Review")
                && diagnostic.offset == Some(offset)), "{diagnostics:?}");
            assert!(invalid[..offset].contains("Zażółć"));
        }
        model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::Cardinality { count: 16 },
            output_collection_variable: "results".into(),
        });
        let xml = export_xml(&model).unwrap();
        let invalid = xml.replacen(">16</bpmn:loopCardinality>", ">256</bpmn:loopCardinality>", 1);
        let offset = invalid.find("<bpmn:loopCardinality").unwrap();
        let (restored, diagnostics) = import_xml(&invalid);
        assert!(restored.is_none());
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
            && diagnostic.element_id.as_deref() == Some("Review")
            && diagnostic.offset == Some(offset)), "{diagnostics:?}");
    }

    #[test]
    fn script_task_xml_preserves_mime_body_mapping_and_di_with_utf8_diagnostics() {
        let mut model = super::super::model::starter_model();
        model.nodes[0].name = "Zażółć 日本語".into();
        model.variables.insert("amount".into(), serde_json::json!(2));
        model.variables.insert("answer".into(), serde_json::Value::Null);
        model.nodes.insert(1, ProcessNode { id: "Script_1".into(), name: "Calculate".into(), repeat: None,
            kind: ProcessNodeKind::ScriptTask { script: "vars.amount + 1".into(),
                output_mapping: BTreeMap::from([("answer".into(), "outputs".into())]) } });
        model.sequence_flows[0].target_id = "Script_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_2".into(),
            source_id: "Script_1".into(), target_id: "End_1".into(), condition: None });
        model.diagram.shapes.push(ProcessShape { element_id: "Script_1".into(),
            x: 180.0, y: 100.0, width: 240.0, height: 96.0 });
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<bpmn:scriptTask id=\"Script_1\" name=\"Calculate\" scriptFormat=\"application/vnd.tentaflow.cel\">"));
        assert!(xml.contains("<tentaflow:scriptTask><tentaflow:outputMapping>{&quot;answer&quot;:&quot;outputs&quot;}</tentaflow:outputMapping></tentaflow:scriptTask>"));
        assert!(xml.contains("<bpmn:script>vars.amount + 1</bpmn:script>"));
        assert!(xml.contains("bpmnElement=\"Script_1\""));
        assert_eq!(import_xml(&xml).0, Some(model.clone()));

        for (invalid, marker) in [
            (xml.replacen("application/vnd.tentaflow.cel", "text/javascript", 1), "<bpmn:scriptTask"),
            (xml.replacen("<bpmn:script>vars.amount + 1</bpmn:script>", "", 1), "<bpmn:scriptTask"),
            (xml.replacen("<bpmn:script>vars.amount + 1</bpmn:script>",
                "<bpmn:script>vars.amount + 1</bpmn:script><bpmn:script>7</bpmn:script>", 1), "<bpmn:script>7"),
            (xml.replacen("vars.amount + 1</bpmn:script>",
                "<bpmn:documentation>nested</bpmn:documentation></bpmn:script>", 1), "<bpmn:documentation>nested"),
            (xml.replacen("{&quot;answer&quot;:&quot;outputs&quot;}", "not-json", 1), "<tentaflow:outputMapping>"),
        ] {
            let (restored, diagnostics) = import_xml(&invalid);
            assert!(restored.is_none());
            let offset = invalid.find(marker).unwrap();
            assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
                && diagnostic.element_id.as_deref() == Some("Script_1")
                && diagnostic.offset == Some(offset)), "{diagnostics:?}");
            assert!(invalid[..offset].contains("Zażółć"));
        }
        let ProcessNodeKind::ScriptTask { output_mapping, .. } = &mut model.nodes[1].kind else { unreachable!() };
        output_mapping.clear();
        let empty_xml = export_xml(&model).unwrap();
        assert!(!empty_xml.contains("<tentaflow:scriptTask>"));
        assert_eq!(import_xml(&empty_xml).0, Some(model));
    }

    #[test]
    fn manual_task_xml_requires_explicit_marker_and_preserves_instructions_and_di() {
        let mut model = super::super::model::starter_model();
        model.nodes[0].name = "Zażółć 日本語".into();
        model.nodes.insert(1, ProcessNode { id: "Manual_1".into(), name: "External work".into(), repeat: None,
            kind: ProcessNodeKind::ManualTask { assignee_user_id: Some("worker-2".into()),
                instructions: "Inspect <report> & register.\nAcknowledge here.".into() } });
        model.sequence_flows[0].target_id = "Manual_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_2".into(),
            source_id: "Manual_1".into(), target_id: "End_1".into(), condition: None });
        model.diagram.shapes.push(ProcessShape { element_id: "Manual_1".into(),
            x: 180.0, y: 100.0, width: 240.0, height: 96.0 });
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<bpmn:manualTask id=\"Manual_1\" name=\"External work\"><bpmn:documentation textFormat=\"text/plain\">"));
        assert!(xml.contains("Inspect &lt;report&gt; &amp; register.\nAcknowledge here.</bpmn:documentation>"));
        assert!(xml.contains("<bpmn:extensionElements><tentaflow:manual assigneeUserId=\"worker-2\"/></bpmn:extensionElements>"));
        assert!(xml.contains("bpmnElement=\"Manual_1\""));
        assert_eq!(import_xml(&xml).0, Some(model.clone()));
        for (invalid, marker) in [
            (xml.replacen("<bpmn:extensionElements><tentaflow:manual assigneeUserId=\"worker-2\"/></bpmn:extensionElements>", "", 1), "<bpmn:manualTask"),
            (xml.replacen("<tentaflow:manual assigneeUserId=\"worker-2\"/>",
                "<tentaflow:manual assigneeUserId=\"worker-2\"/><tentaflow:manual/>", 1), "<tentaflow:manual/>"),
            (xml.replacen("<tentaflow:manual assigneeUserId=\"worker-2\"/>", "<bpmn:manual/>", 1), "<bpmn:manual/>"),
            (xml.replacen("<tentaflow:manual assigneeUserId=\"worker-2\"/>",
                "<tentaflow:manual><bpmn:documentation>nested</bpmn:documentation></tentaflow:manual>", 1), "<tentaflow:manual>"),
        ] {
            let (restored, diagnostics) = import_xml(&invalid);
            assert!(restored.is_none());
            let offset = invalid.find(marker).unwrap();
            assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
                && diagnostic.element_id.as_deref() == Some("Manual_1")
                && diagnostic.offset == Some(offset)), "{diagnostics:?}");
            assert!(invalid[..offset].contains("Zażółć"));
        }
        let ProcessNodeKind::ManualTask { assignee_user_id, .. } = &mut model.nodes[1].kind else { unreachable!() };
        *assignee_user_id = None;
        let default_xml = export_xml(&model).unwrap();
        assert!(default_xml.contains("<tentaflow:manual/>"));
        assert_eq!(import_xml(&default_xml).0, Some(model));
    }

    #[test]
    fn embedded_manual_and_receive_boundaries_preserve_markers_qnames_di_and_causal_offsets() {
        let mut model = super::super::model::starter_model();
        model.nodes[0].name = "Zażółć 日本語".into();
        model.timer_timezone = Some("UTC".into());
        model.messages.push(ProcessMessageDeclaration {
            message_id: "Message_1".into(), name: "order.received".into(),
        });
        let nodes = vec![
            ProcessNode { id: "LocalStart".into(), name: "Start".into(), repeat: None,
                kind: ProcessNodeKind::Start },
            ProcessNode { id: "Manual_1".into(), name: "External work".into(), repeat: None,
                kind: ProcessNodeKind::ManualTask { assignee_user_id: None,
                    instructions: "Check <register> & acknowledge".into() } },
            ProcessNode { id: "Receive_1".into(), name: "Wait for delivery".into(), repeat: None,
                kind: ProcessNodeKind::ReceiveTask { message_ref: "Message_1".into(),
                    correlation_expression: "vars.case_key".into(),
                    output_mapping: BTreeMap::from([("received_payload".into(), "outputs".into())]) } },
            ProcessNode { id: "LocalEnd".into(), name: "End".into(), repeat: None,
                kind: ProcessNodeKind::End },
            ProcessNode { id: "Manual_Timer".into(), name: "Manual deadline".into(), repeat: None,
                kind: ProcessNodeKind::BoundaryTimer { attached_to_id: "Manual_1".into(),
                    cancel_activity: true, timer: ProcessTimerSpec::Duration { seconds: 60 } } },
            ProcessNode { id: "Manual_Message".into(), name: "Manual reply".into(), repeat: None,
                kind: ProcessNodeKind::BoundaryMessage { attached_to_id: "Manual_1".into(),
                    cancel_activity: false, message_ref: "Message_1".into(),
                    correlation_expression: "vars.case_key".into(),
                    output_mapping: BTreeMap::from([("boundary_payload".into(), "outputs".into())]) } },
            ProcessNode { id: "Receive_Timer".into(), name: "Receive deadline".into(), repeat: None,
                kind: ProcessNodeKind::BoundaryTimer { attached_to_id: "Receive_1".into(),
                    cancel_activity: true, timer: ProcessTimerSpec::Duration { seconds: 90 } } },
            ProcessNode { id: "Receive_Message".into(), name: "Receive reply".into(), repeat: None,
                kind: ProcessNodeKind::BoundaryMessage { attached_to_id: "Receive_1".into(),
                    cancel_activity: false, message_ref: "Message_1".into(),
                    correlation_expression: "vars.case_key".into(),
                    output_mapping: BTreeMap::from([("boundary_payload".into(), "outputs".into())]) } },
        ];
        let mut flows = Vec::new();
        for (id, source_id, target_id) in [
            ("LocalFlow1", "LocalStart", "Manual_1"),
            ("LocalFlow2", "Manual_1", "Receive_1"),
            ("LocalFlow3", "Receive_1", "LocalEnd"),
            ("ManualTimerFlow", "Manual_Timer", "LocalEnd"),
            ("ManualMessageFlow", "Manual_Message", "LocalEnd"),
            ("ReceiveTimerFlow", "Receive_Timer", "LocalEnd"),
            ("ReceiveMessageFlow", "Receive_Message", "LocalEnd"),
        ] {
            flows.push(ProcessSequenceFlow { id: id.into(), source_id: source_id.into(),
                target_id: target_id.into(), condition: None });
        }
        let shapes = nodes.iter().enumerate().map(|(index, node)| {
            let event = matches!(node.kind, ProcessNodeKind::Start | ProcessNodeKind::End
                | ProcessNodeKind::BoundaryTimer { .. } | ProcessNodeKind::BoundaryMessage { .. });
            ProcessShape { element_id: node.id.clone(), x: 40.0 + index as f64 * 60.0, y: 60.0,
                width: if event { 56.0 } else { 160.0 },
                height: if event { 56.0 } else { 96.0 } }
        }).collect();
        let edges = flows.iter().map(|flow| ProcessEdgeDiagram {
            sequence_flow_id: flow.id.clone(), waypoints: vec![
                ProcessPoint { x: 100.0, y: 108.0 }, ProcessPoint { x: 160.0, y: 108.0 },
            ],
        }).collect();
        model.nodes.insert(1, ProcessNode { id: "Sub_1".into(), name: "External & wait".into(),
            repeat: None, kind: ProcessNodeKind::SubProcess {
                body: ProcessSubProcess { nodes, sequence_flows: flows,
                    variables: BTreeMap::from([
                        ("case_key".into(), serde_json::json!("case-1")),
                        ("received_payload".into(), serde_json::Value::Null),
                        ("boundary_payload".into(), serde_json::Value::Null),
                    ]), diagram: ProcessDiagram { shapes, edges } },
                input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
            } });
        model.sequence_flows[0].target_id = "Sub_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "RootFlow2".into(),
            source_id: "Sub_1".into(), target_id: "End_1".into(), condition: None });
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<tentaflow:manual/>"));
        assert!(xml.contains("Check &lt;register&gt; &amp; acknowledge"));
        assert!(xml.contains("<tentaflow:receiveTask>"));
        assert!(xml.contains("messageRef=\"tns:Message_1\""));
        for (id, target) in [("Manual_Timer", "Manual_1"),
            ("Manual_Message", "Manual_1"), ("Receive_Timer", "Receive_1"),
            ("Receive_Message", "Receive_1")] {
            let start = xml.find(&format!("<bpmn:boundaryEvent id=\"{id}\"")).unwrap();
            let end = start + xml[start..].find("</bpmn:boundaryEvent>").unwrap();
            assert!(xml[start..end].contains(&format!("attachedToRef=\"{target}\"")));
            assert!(xml.contains(&format!("bpmnElement=\"{id}\"")));
        }
        assert_eq!(import_xml(&xml).0, Some(model));
        for (id, target) in [("Manual_Timer", "Manual_1"),
            ("Receive_Timer", "Receive_1")] {
            let opening = format!("<bpmn:boundaryEvent id=\"{id}\"");
            let start = xml.find(&opening).unwrap();
            let old = format!("attachedToRef=\"{target}\"");
            for replacement in ["", "attachedToRef=\"MissingActivity\""] {
                let mut invalid = xml.clone();
                let local = start + invalid[start..].find(&old).unwrap();
                invalid.replace_range(local..local + old.len(), replacement);
                let offset = invalid.find(&opening).unwrap();
                assert!(invalid[..offset].contains("Zażółć 日本語"));
                assert!(invalid[..offset].chars().count() < offset);
                let (restored, diagnostics) = import_xml(&invalid);
                assert!(restored.is_none());
                assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
                    && diagnostic.element_id.as_deref() == Some(id)
                    && diagnostic.offset == Some(offset)), "{diagnostics:?}");
            }
        }
    }

    #[test]
    fn send_receive_task_xml_requires_distinct_markers_and_preserves_di() {
        let mut model = super::super::model::starter_model();
        model.nodes[0].name = "Zażółć 日本語".into();
        model.timer_timezone = Some("UTC".into());
        model.messages.push(tentaflow_protocol::processes::ProcessMessageDeclaration {
            message_id: "Message_1".into(), name: "order.received".into(),
        });
        model.nodes.insert(1, ProcessNode { id: "Send_1".into(), name: "Admit <&>".into(), repeat: None,
            kind: ProcessNodeKind::SendTask { message_ref: "Message_1".into(),
                target: ProcessMessageTargetSpec::Start { definition_id: uuid::Uuid::nil().to_string() },
                correlation_expression: "vars.case_key".into(), payload_expression: "vars.payload".into(),
                ttl_seconds: 60 } });
        model.nodes.insert(2, ProcessNode { id: "Receive_1".into(), name: "Wait <&>".into(), repeat: None,
            kind: ProcessNodeKind::ReceiveTask { message_ref: "Message_1".into(),
                correlation_expression: "vars.case_key".into(), output_mapping: BTreeMap::new() } });
        model.nodes.push(ProcessNode { id: "Send_Timer".into(), name: "Deadline".into(), repeat: None,
            kind: ProcessNodeKind::BoundaryTimer { attached_to_id: "Send_1".into(), cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 60 } } });
        model.nodes.push(ProcessNode { id: "Send_Message".into(), name: "Reply".into(), repeat: None,
            kind: ProcessNodeKind::BoundaryMessage { attached_to_id: "Send_1".into(), cancel_activity: false,
                message_ref: "Message_1".into(), correlation_expression: "vars.case_key".into(),
                output_mapping: BTreeMap::new() } });
        model.sequence_flows[0].target_id = "Send_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_Send".into(),
            source_id: "Send_1".into(), target_id: "Receive_1".into(), condition: None });
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_Receive".into(),
            source_id: "Receive_1".into(), target_id: "End_1".into(), condition: None });
        for (id, source_id) in [("Flow_Timer", "Send_Timer"), ("Flow_Message", "Send_Message")] {
            model.sequence_flows.push(ProcessSequenceFlow { id: id.into(), source_id: source_id.into(),
                target_id: "End_1".into(), condition: None });
        }
        model.variables.insert("case_key".into(), serde_json::json!("case-1"));
        model.variables.insert("payload".into(), serde_json::json!({"business_key": 1}));
        model.diagram.shapes.push(ProcessShape { element_id: "Send_1".into(),
            x: 180.0, y: 100.0, width: 240.0, height: 96.0 });
        model.diagram.shapes.push(ProcessShape { element_id: "Receive_1".into(),
            x: 460.0, y: 100.0, width: 240.0, height: 96.0 });
        model.diagram.shapes.push(ProcessShape { element_id: "Send_Timer".into(),
            x: 360.0, y: 170.0, width: 56.0, height: 56.0 });
        model.diagram.shapes.push(ProcessShape { element_id: "Send_Message".into(),
            x: 180.0, y: 170.0, width: 56.0, height: 56.0 });
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<bpmn:sendTask id=\"Send_1\" name=\"Admit &lt;&amp;&gt;\" messageRef=\"tns:Message_1\" implementation=\"##unspecified\">"));
        assert!(xml.contains("<bpmn:receiveTask id=\"Receive_1\" name=\"Wait &lt;&amp;&gt;\" messageRef=\"tns:Message_1\" implementation=\"##unspecified\" instantiate=\"false\">"));
        assert!(xml.contains("<tentaflow:sendTask>") && xml.contains("<tentaflow:receiveTask>"));
        assert!(xml.contains("bpmnElement=\"Send_1\"") && xml.contains("bpmnElement=\"Receive_1\""));
        assert!(xml.contains("bpmnElement=\"Send_Timer\"") && xml.contains("bpmnElement=\"Send_Message\""));
        for (id, definition) in [("Send_Message", "messageEventDefinition"),
            ("Send_Timer", "timerEventDefinition")] {
            let start = xml.find(&format!("<bpmn:boundaryEvent id=\"{id}\"")).unwrap();
            let end = start + xml[start..].find("</bpmn:boundaryEvent>").unwrap();
            let event = &xml[start..end];
            if id == "Send_Message" {
                assert!(event.find("<bpmn:extensionElements>").unwrap()
                    < event.find(&format!("<bpmn:{definition}")).unwrap());
            }
            assert!(event.contains("attachedToRef=\"Send_1\""));
        }
        assert_eq!(import_xml(&xml).0, Some(model));
        for invalid_attachment in [
            xml.replacen(" attachedToRef=\"Send_1\"", "", 1),
            xml.replacen("attachedToRef=\"Send_1\"", "attachedToRef=\"MissingActivity\"", 1),
        ] {
            let expected_offset = invalid_attachment.find("<bpmn:boundaryEvent id=\"Send_Timer\"").unwrap();
            assert!(invalid_attachment[..expected_offset].contains("Zażółć 日本語"));
            assert!(invalid_attachment[..expected_offset].chars().count() < expected_offset);
            let (restored, diagnostics) = import_xml(&invalid_attachment);
            assert!(restored.is_none());
            assert_eq!(diagnostics.len(), 1);
            let diagnostic = &diagnostics[0];
            assert!(diagnostic.fatal);
            assert_eq!(diagnostic.code, "UNSUPPORTED_OR_INVALID_BPMN");
            assert_eq!(diagnostic.element_id.as_deref(), Some("Send_Timer"));
            assert_eq!(diagnostic.offset, Some(expected_offset));
        }
        let send_extension_start = xml.find("<bpmn:extensionElements><tentaflow:sendTask>").unwrap();
        let send_extension_end = send_extension_start
            + xml[send_extension_start..].find("</bpmn:extensionElements>").unwrap()
            + "</bpmn:extensionElements>".len();
        let mut missing = xml.clone();
        missing.replace_range(send_extension_start..send_extension_end, "");
        let duplicate = xml.replacen("</tentaflow:sendTask>",
            "</tentaflow:sendTask><tentaflow:sendTask>{}</tentaflow:sendTask>", 1);
        let nested = xml.replacen("</tentaflow:receiveTask>",
            "<bpmn:documentation>nested</bpmn:documentation></tentaflow:receiveTask>", 1);
        for (invalid, id, marker) in [
            (missing, "Send_1", "<bpmn:sendTask"),
            (duplicate, "Send_1", "<tentaflow:sendTask>{}"),
            (nested, "Receive_1", "<tentaflow:receiveTask>"),
            (xml.replacen("<tentaflow:sendTask>", "<tentaflow:receiveTask>", 1)
                .replacen("</tentaflow:sendTask>", "</tentaflow:receiveTask>", 1), "Send_1", "<tentaflow:receiveTask>"),
            (xml.replacen("<tentaflow:receiveTask>", "<tentaflow:sendTask>", 1)
                .replacen("</tentaflow:receiveTask>", "</tentaflow:sendTask>", 1), "Receive_1", "<tentaflow:sendTask>"),
            (xml.replacen("instantiate=\"false\"", "instantiate=\"true\"", 1), "Receive_1", "<bpmn:receiveTask"),
            (xml.replacen("messageRef=\"tns:Message_1\"", "messageRef=\"bpmn:Message_1\"", 1), "Send_1", "<bpmn:sendTask"),
            (xml.replacen("messageRef=\"tns:Message_1\"", "messageRef=\"Message_1\"", 1), "Send_1", "<bpmn:sendTask"),
            (xml.replacen("messageRef=\"tns:Message_1\" implementation=\"##unspecified\"",
                "messageRef=\"tns:Message_1\" operationRef=\"tns:Operation_1\" implementation=\"##unspecified\"", 1), "Send_1", "<bpmn:sendTask"),
        ] {
            let (restored, diagnostics) = import_xml(&invalid);
            assert!(restored.is_none());
            let task = if id == "Send_1" { "<bpmn:sendTask id=\"Send_1\"" }
                else { "<bpmn:receiveTask id=\"Receive_1\"" };
            let task_offset = invalid.find(task).unwrap();
            let marker_offset = task_offset + invalid[task_offset..].find(marker).unwrap();
            assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
                && diagnostic.element_id.as_deref() == Some(id)
                && diagnostic.offset == Some(marker_offset)), "{diagnostics:?}");
        }
    }

    #[test]
    fn declarations_require_a_namespace_with_definitions_byte_context() {
        let mut model = super::super::model::starter_model();
        model.target_namespace = Some("urn:orders".into());
        model.signals.push(ProcessSignalDeclaration {
            signal_id: "Signal_1".into(), namespace_uri: "urn:orders".into(), name: "Order changed".into(),
        });
        model.nodes.insert(1, ProcessNode { id: "Throw_1".into(), name: "Admit".into(), repeat: None,
            kind: ProcessNodeKind::SignalThrow { signal_ref: "Signal_1".into(),
                payload_expression: "vars.payload".into(), ttl_seconds: 60 } });
        model.variables.insert("payload".into(), serde_json::json!({"business_key": 1}));
        model.sequence_flows[0].target_id = "Throw_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_2".into(),
            source_id: "Throw_1".into(), target_id: "End_1".into(), condition: None });
        let exported = export_xml(&model).unwrap();
        for (with_id, expected_id) in [(false, None), (true, Some("Definitions_1"))] {
            let xml = if with_id { exported.replacen("<bpmn:definitions ",
                "<bpmn:definitions id=\"Definitions_1\" ", 1) } else { exported.clone() };
            let prefixed = xml.replacen("?><bpmn:definitions",
                "?><!-- Zażółć 日本語 --><bpmn:definitions", 1);
            assert_eq!(import_xml(&prefixed).0, Some(model.clone()));
            for invalid in [
                prefixed.replacen(" targetNamespace=\"urn:orders\"", "", 1),
                prefixed.replacen(" targetNamespace=\"urn:orders\"", " targetNamespace=\"\"", 1),
            ] {
                let offset = invalid.find("<bpmn:definitions").unwrap();
                assert!(invalid[..offset].contains("Zażółć 日本語"));
                let (restored, diagnostics) = import_xml(&invalid);
                assert!(restored.is_none());
                assert_eq!(diagnostics.len(), 1);
                let diagnostic = &diagnostics[0];
                assert!(diagnostic.fatal);
                assert_eq!(diagnostic.code, "UNSUPPORTED_OR_INVALID_BPMN");
                assert_eq!(diagnostic.message, "BPMN declarations require explicit targetNamespace");
                assert_eq!(diagnostic.element_id.as_deref(), expected_id);
                assert_eq!(diagnostic.offset, Some(offset));
            }
        }
        let plain = export_xml(&super::super::model::starter_model()).unwrap();
        let legacy = plain.replacen(" targetNamespace=\"https://tentaflow.app/bpmn/1\"", "", 1);
        assert_ne!(legacy, plain);
        assert_eq!(import_xml(&legacy).0, Some(super::super::model::starter_model()));
    }

    #[test]
    fn signal_events_require_declared_namespace_exact_markers_and_preserve_di() {
        let mut model = super::super::model::starter_model();
        model.nodes[0].name = "Zażółć 日本語".into();
        model.target_namespace = Some("urn:orders".into());
        model.signals.push(tentaflow_protocol::processes::ProcessSignalDeclaration {
            signal_id: "Signal_1".into(), namespace_uri: "urn:orders".into(), name: "Order <&>".into(),
        });
        model.variables.insert("payload".into(), serde_json::json!({"business_key": 1}));
        model.nodes.insert(1, ProcessNode { id: "Throw_1".into(), name: "Admit".into(), repeat: None,
            kind: ProcessNodeKind::SignalThrow { signal_ref: "Signal_1".into(),
                payload_expression: "vars.payload".into(), ttl_seconds: 60 } });
        model.nodes.insert(2, ProcessNode { id: "Catch_1".into(), name: "Wait".into(), repeat: None,
            kind: ProcessNodeKind::SignalCatch { signal_ref: "Signal_1".into(),
                output_mapping: BTreeMap::new() } });
        model.sequence_flows[0].target_id = "Throw_1".into();
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_Throw".into(),
            source_id: "Throw_1".into(), target_id: "Catch_1".into(), condition: None });
        model.sequence_flows.push(ProcessSequenceFlow { id: "Flow_Catch".into(),
            source_id: "Catch_1".into(), target_id: "End_1".into(), condition: None });
        model.diagram.shapes.push(ProcessShape { element_id: "Throw_1".into(),
            x: 180.0, y: 100.0, width: 56.0, height: 56.0 });
        model.diagram.shapes.push(ProcessShape { element_id: "Catch_1".into(),
            x: 400.0, y: 100.0, width: 56.0, height: 56.0 });
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<bpmn:signal id=\"Signal_1\" name=\"Order &lt;&amp;&gt;\"/>"));
        assert!(xml.contains("<bpmn:signalEventDefinition signalRef=\"tns:Signal_1\"/>"));
        assert!(xml.contains("<tentaflow:signalThrow>") && xml.contains("<tentaflow:signalCatch>"));
        for (id, event_tag, marker) in [
            ("Throw_1", "intermediateThrowEvent", "signalThrow"),
            ("Catch_1", "intermediateCatchEvent", "signalCatch"),
        ] {
            let opening = format!("<bpmn:{event_tag} id=\"{id}\"");
            let start = xml.find(&opening).unwrap();
            let end = start + xml[start..].find(&format!("</bpmn:{event_tag}>")).unwrap();
            let event = &xml[start..end];
            let extension = event.find("<bpmn:extensionElements>").unwrap();
            let config = event.find(&format!("<tentaflow:{marker}>")).unwrap();
            let definition = event.find("<bpmn:signalEventDefinition").unwrap();
            assert!(extension < config && config < definition, "{id} has invalid BPMN element order");
        }
        assert!(xml.contains("bpmnElement=\"Throw_1\"") && xml.contains("bpmnElement=\"Catch_1\""));
        assert_eq!(import_xml(&xml).0, Some(model));
        let structured = xml.replacen("<bpmn:signal id=\"Signal_1\"", "<bpmn:signal id=\"Signal_1\" structureRef=\"tns:Payload\"", 1);
        let (restored, diagnostics) = import_xml(&structured);
        assert!(restored.is_none());
        let declaration_offset = structured.find("<bpmn:signal id=\"Signal_1\"").unwrap();
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
            && diagnostic.element_id.as_deref() == Some("Signal_1")
            && diagnostic.offset == Some(declaration_offset)), "{diagnostics:?}");
        let alternate_prefix = xml.replace("xmlns:tns=", "xmlns:orders=")
            .replace("tns:Signal_1", "orders:Signal_1");
        assert!(import_xml(&alternate_prefix).0.is_some());
        for (invalid, id, marker) in [
            (xml.replacen("signalRef=\"tns:Signal_1\"", "signalRef=\"Signal_1\"", 1), "Throw_1", "<bpmn:signalEventDefinition"),
            (xml.replacen("signalRef=\"tns:Signal_1\"", "signalRef=\"bpmn:Signal_1\"", 1), "Throw_1", "<bpmn:signalEventDefinition"),
            (xml.replacen("<tentaflow:signalThrow>", "<tentaflow:signalCatch>", 1)
                .replacen("</tentaflow:signalThrow>", "</tentaflow:signalCatch>", 1), "Throw_1", "<tentaflow:signalCatch>"),
            (xml.replacen("<tentaflow:signalCatch>", "<bpmn:signalCatch>", 1)
                .replacen("</tentaflow:signalCatch>", "</bpmn:signalCatch>", 1), "Catch_1", "<bpmn:signalCatch>"),
            (xml.replacen("</tentaflow:signalCatch>", "</tentaflow:signalCatch><tentaflow:signalCatch>{}</tentaflow:signalCatch>", 1), "Catch_1", "<tentaflow:signalCatch>{}"),
        ] {
            let (restored, diagnostics) = import_xml(&invalid);
            assert!(restored.is_none());
            let opening = format!("<bpmn:intermediate{}Event id=\"{id}\"",
                if id == "Throw_1" { "Throw" } else { "Catch" });
            let start = invalid.find(&opening).unwrap();
            let offset = start + invalid[start..].find(marker).unwrap();
            assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
                && diagnostic.element_id.as_deref() == Some(id)
                && diagnostic.offset == Some(offset)), "{diagnostics:?}");
            assert!(invalid[..offset].contains("Zażółć"));
        }
        for (invalid, id) in [
            (xml.replace("<bpmn:intermediateCatchEvent id=\"Catch_1\"", "<bpmn:startEvent id=\"Catch_1\"")
                .replace("</bpmn:intermediateCatchEvent>", "</bpmn:startEvent>"), "Catch_1"),
            (xml.replace("<bpmn:intermediateCatchEvent id=\"Catch_1\"", "<bpmn:boundaryEvent id=\"Catch_1\" attachedToRef=\"Throw_1\"")
                .replace("</bpmn:intermediateCatchEvent>", "</bpmn:boundaryEvent>"), "Catch_1"),
        ] {
            let catch_start = invalid.find(&format!("id=\"{id}\"")).unwrap();
            let offset = catch_start + invalid[catch_start..].find("<bpmn:signalEventDefinition").unwrap();
            let (restored, diagnostics) = import_xml(&invalid);
            assert!(restored.is_none());
            assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
                && diagnostic.element_id.as_deref() == Some(id)
                && diagnostic.offset == Some(offset)), "{diagnostics:?}");
        }
    }

    #[test]
    fn event_gateway_receive_signal_and_timer_xml_round_trips_with_causal_profile_diagnostics() {
        let mut model = super::super::model::starter_model();
        model.nodes[0].name = "Zażółć 日本語".into();
        model.target_namespace = Some("urn:orders".into());
        model.timer_timezone = Some("UTC".into());
        model.messages.push(ProcessMessageDeclaration {
            message_id: "Message_1".into(), name: "order.received".into(),
        });
        model.signals.push(ProcessSignalDeclaration {
            signal_id: "Signal_1".into(), namespace_uri: "urn:orders".into(),
            name: "Order changed".into(),
        });
        model.variables.insert("case_key".into(), serde_json::json!("case-1"));
        model.variables.insert("received".into(), serde_json::Value::Null);
        model.nodes.extend([
            ProcessNode { id: "Race_1".into(), name: "First event".into(), repeat: None,
                kind: ProcessNodeKind::EventBasedGateway },
            ProcessNode { id: "Receive_1".into(), name: "Receive".into(), repeat: None,
                kind: ProcessNodeKind::ReceiveTask {
                    message_ref: "Message_1".into(),
                    correlation_expression: "vars.case_key".into(),
                    output_mapping: BTreeMap::from([("received".into(), "outputs".into())]),
                } },
            ProcessNode { id: "Signal_1_Catch".into(), name: "Signal".into(), repeat: None,
                kind: ProcessNodeKind::SignalCatch { signal_ref: "Signal_1".into(),
                    output_mapping: BTreeMap::new() } },
            ProcessNode { id: "Timer_1".into(), name: "Timeout".into(), repeat: None,
                kind: ProcessNodeKind::TimerCatch {
                    timer: ProcessTimerSpec::Duration { seconds: 60 },
                } },
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
            model.sequence_flows.push(ProcessSequenceFlow { id: id.into(),
                source_id: source.into(), target_id: target.into(), condition: None });
        }
        for (index, id) in ["Race_1", "Receive_1", "Signal_1_Catch", "Timer_1"].iter().enumerate() {
            model.diagram.shapes.push(ProcessShape { element_id: (*id).into(),
                x: 160.0 + index as f64 * 120.0, y: 140.0,
                width: if *id == "Race_1" { 72.0 } else { 56.0 }, height: 56.0 });
        }
        model.diagram.edges = model.sequence_flows.iter().map(|flow| ProcessEdgeDiagram {
            sequence_flow_id: flow.id.clone(), waypoints: vec![
                ProcessPoint { x: 100.0, y: 188.0 }, ProcessPoint { x: 220.0, y: 188.0 },
            ],
        }).collect();
        let xml = export_xml(&model).unwrap();
        assert!(xml.contains("<bpmn:eventBasedGateway id=\"Race_1\""));
        assert!(xml.contains("messageRef=\"tns:Message_1\""));
        assert!(xml.contains("signalRef=\"tns:Signal_1\""));
        for id in ["Race_1", "Receive_1", "Signal_1_Catch", "Timer_1"] {
            assert!(xml.contains(&format!("bpmnElement=\"{id}\"")));
        }
        for flow in &model.sequence_flows {
            assert!(xml.contains(&format!("bpmnElement=\"{}\"", flow.id)));
        }
        assert_eq!(import_xml(&xml).0, Some(model));
        let malformed_qname = xml.replacen("signalRef=\"tns:Signal_1\"",
            "signalRef=\"Signal_1\"", 1);
        let (restored, diagnostics) = import_xml(&malformed_qname);
        assert!(restored.is_none());
        let offset = malformed_qname.find("<bpmn:signalEventDefinition").unwrap();
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
            && diagnostic.element_id.as_deref() == Some("Signal_1_Catch")
            && diagnostic.offset == Some(offset)), "{diagnostics:?}");

        let opening = "<bpmn:intermediateCatchEvent id=\"Signal_1_Catch\"";
        let start = xml.find(opening).unwrap();
        let end = start + xml[start..].find("</bpmn:intermediateCatchEvent>").unwrap()
            + "</bpmn:intermediateCatchEvent>".len();
        let message_config = escaped(&serde_json::to_string(&serde_json::json!({
            "correlation_expression": "vars.case_key", "output_mapping": {},
        })).unwrap());
        let message_catch = format!(
            "<bpmn:intermediateCatchEvent id=\"Signal_1_Catch\" name=\"Signal\"><bpmn:extensionElements><tentaflow:message>{message_config}</tentaflow:message></bpmn:extensionElements><bpmn:messageEventDefinition messageRef=\"tns:Message_1\"/></bpmn:intermediateCatchEvent>"
        );
        let mut mixed = xml.clone();
        mixed.replace_range(start..end, &message_catch);
        mixed = mixed.replace("<bpmn:signal id=\"Signal_1\" name=\"Order changed\"/>", "");
        let (restored, diagnostics) = import_xml(&mixed);
        assert!(restored.is_none());
        let offset = mixed.find("<bpmn:eventBasedGateway id=\"Race_1\"").unwrap();
        assert!(mixed[..offset].chars().count() < offset);
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
            && diagnostic.element_id.as_deref() == Some("Race_1")
            && diagnostic.offset == Some(offset)
            && diagnostic.message.contains("cannot mix ReceiveTask with MessageCatch")),
            "{diagnostics:?}");

        let boundary = "<bpmn:boundaryEvent id=\"Receive_Boundary\" attachedToRef=\"Receive_1\" cancelActivity=\"true\"><bpmn:timerEventDefinition><bpmn:timeDuration>PT30S</bpmn:timeDuration></bpmn:timerEventDefinition></bpmn:boundaryEvent>";
        let boundary_flow = "<bpmn:sequenceFlow id=\"Boundary_End\" sourceRef=\"Receive_Boundary\" targetRef=\"End_1\"/>";
        let attached = xml.replacen("</bpmn:process>",
            &format!("{boundary}{boundary_flow}</bpmn:process>"), 1);
        let (restored, diagnostics) = import_xml(&attached);
        assert!(restored.is_none());
        let offset = attached.find("<bpmn:boundaryEvent id=\"Receive_Boundary\"").unwrap();
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
            && diagnostic.element_id.as_deref() == Some("Receive_Boundary")
            && diagnostic.offset == Some(offset)
            && diagnostic.message.contains("cannot have attached boundary")),
            "{diagnostics:?}");

        let extra = xml.replacen("</bpmn:process>",
            "<bpmn:sequenceFlow id=\"Extra_Receive\" sourceRef=\"Signal_1_Catch\" targetRef=\"Receive_1\"/></bpmn:process>", 1);
        let (restored, diagnostics) = import_xml(&extra);
        assert!(restored.is_none());
        let offset = extra.find("<bpmn:receiveTask id=\"Receive_1\"").unwrap();
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.fatal
            && diagnostic.element_id.as_deref() == Some("Receive_1")
            && diagnostic.offset == Some(offset)
            && diagnostic.message.contains("must have one incoming and outgoing flow")),
            "{diagnostics:?}");
    }

}
