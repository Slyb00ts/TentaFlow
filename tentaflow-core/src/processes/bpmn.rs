// ============ File: bpmn.rs — bounded BPMN B1 XML import and export ============

use std::collections::{BTreeMap, HashMap};

use anyhow::{bail, ensure, Context, Result};
use quick_xml::events::Event;
use quick_xml::name::{QName, ResolveResult};
use quick_xml::{NsReader, XmlVersion};
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessDiagnostic, ProcessDiagram, ProcessEdgeDiagram, ProcessModel,
    ProcessNode, ProcessNodeKind, ProcessPoint, ProcessSequenceFlow, ProcessShape,
};

use super::model::{validate_model, validate_variables, MAX_MODEL_BYTES, MAX_VARIABLE_BYTES};

const BPMN: &str = "http://www.omg.org/spec/BPMN/20100524/MODEL";
const BPMNDI: &str = "http://www.omg.org/spec/BPMN/20100524/DI";
const DC: &str = "http://www.omg.org/spec/DD/20100524/DC";
const DI: &str = "http://www.omg.org/spec/DD/20100524/DI";
const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const TF: &str = "https://tentaflow.app/bpmn/1";

#[derive(Debug)]
struct Element {
    ns: String,
    local: String,
    attrs: HashMap<(String, String), String>,
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
                let element = Element {
                    ns,
                    local,
                    attrs,
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

fn process_variables(
    process: &Element,
    process_id: &str,
) -> Result<BTreeMap<String, serde_json::Value>> {
    let Some(extension) = process.child(BPMN, "extensionElements")? else {
        return Ok(BTreeMap::new());
    };
    extension.attrs_only(&[])?;
    extension.children_only(&[(TF, "variables")])?;
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
    serde_json::from_value(value).map_err(|error| invalid(error.to_string()).into())
}

fn node_from_xml(element: &Element) -> Result<ProcessNode> {
    let id = element.required("id")?;
    let name = element.attr("name").unwrap_or_default().to_string();
    let kind = match element.local.as_str() {
        "startEvent" => {
            element.attrs_only(&["id", "name"])?;
            element.children_only(&[])?;
            ProcessNodeKind::Start
        }
        "endEvent" => {
            element.attrs_only(&["id", "name"])?;
            element.children_only(&[])?;
            ProcessNodeKind::End
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
            ])?;
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
            }
        }
        other => bail!("unsupported BPMN node {other} at byte {}", element.offset),
    };
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
        child.attrs_only(&["id", "bpmnElement"])?;
        if child.is(BPMNDI, "BPMNShape") {
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

fn parse_model(xml: &str) -> Result<ProcessModel> {
    let root = parse_tree(xml)?;
    ensure!(
        root.is(BPMN, "definitions"),
        "BPMN root must use the BPMN model namespace"
    );
    root.attrs_only(&["id", "targetNamespace"])?;
    root.children_only(&[(BPMN, "process"), (BPMNDI, "BPMNDiagram")])?;
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
    process.children_only(&[
        (BPMN, "extensionElements"),
        (BPMN, "startEvent"),
        (BPMN, "endEvent"),
        (BPMN, "userTask"),
        (BPMN, "serviceTask"),
        (BPMN, "exclusiveGateway"),
        (BPMN, "parallelGateway"),
        (BPMN, "sequenceFlow"),
    ])?;
    let process_id = process.required("id")?;
    let variables = process_variables(process, &process_id)?;
    let mut nodes = Vec::new();
    let mut sequence_flows = Vec::new();
    for child in &process.children {
        if child.is(BPMN, "extensionElements") {
            continue;
        }
        if child.is(BPMN, "sequenceFlow") {
            child.attrs_only(&["id", "sourceRef", "targetRef"])?;
            child.children_only(&[(BPMN, "conditionExpression")])?;
            ensure!(
                child.children.len() <= 1,
                "sequence flow has multiple conditions"
            );
            let condition = child
                .child(BPMN, "conditionExpression")?
                .map(|condition| {
                    condition.attrs_only(&["language"])?;
                    if let Some(language) = condition.attr("language") {
                        ensure!(
                            language == "https://cel.dev/spec",
                            "unsupported expression language"
                        );
                    }
                    ensure!(
                        condition.children.is_empty(),
                        "condition expression must be text"
                    );
                    Ok(condition.text.clone())
                })
                .transpose()?;
            sequence_flows.push(ProcessSequenceFlow {
                id: child.required("id")?,
                source_id: child.required("sourceRef")?,
                target_id: child.required("targetRef")?,
                condition,
            });
        } else {
            nodes.push(node_from_xml(child)?);
        }
    }
    let diagrams: Vec<_> = root
        .children
        .iter()
        .filter(|child| child.is(BPMNDI, "BPMNDiagram"))
        .collect();
    ensure!(diagrams.len() <= 1, "B1 XML supports one BPMN diagram");
    let diagram = diagrams
        .first()
        .map(|element| diagram_from_xml(element, &process_id))
        .transpose()?
        .unwrap_or_default();
    let model = ProcessModel {
        schema_version: 1,
        process_id,
        nodes,
        sequence_flows,
        variables,
        diagram,
    };
    validate_model(&model)?;
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

pub fn export_xml(model: &ProcessModel) -> Result<String> {
    validate_model(model)?;
    let mut xml=format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><bpmn:definitions xmlns:bpmn=\"{BPMN}\" xmlns:bpmndi=\"{BPMNDI}\" xmlns:dc=\"{DC}\" xmlns:di=\"{DI}\" xmlns:tentaflow=\"{TF}\" targetNamespace=\"{TF}\"><bpmn:process id=\"{}\" isExecutable=\"true\">",escaped(&model.process_id));
    xml.push_str(&format!(
        "<bpmn:extensionElements><tentaflow:variables>{}</tentaflow:variables></bpmn:extensionElements>",
        escaped(&serde_json::to_string(&model.variables)?)
    ));
    for node in &model.nodes {
        let (tag, extra) = match &node.kind {
            ProcessNodeKind::Start => ("startEvent", String::new()),
            ProcessNodeKind::End => ("endEvent", String::new()),
            ProcessNodeKind::ParallelGateway => ("parallelGateway", String::new()),
            ProcessNodeKind::ExclusiveGateway { default_flow_id } => (
                "exclusiveGateway",
                default_flow_id
                    .as_ref()
                    .map(|id| format!(" default=\"{}\"", escaped(id)))
                    .unwrap_or_default(),
            ),
            ProcessNodeKind::UserTask { .. } => ("userTask", String::new()),
            ProcessNodeKind::ServiceTask { .. } => ("serviceTask", String::new()),
        };
        xml.push_str(&format!(
            "<bpmn:{tag} id=\"{}\" name=\"{}\"{extra}",
            escaped(&node.id),
            escaped(&node.name)
        ));
        match &node.kind {
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
                xml.push_str("</tentaflow:service></bpmn:extensionElements>");
                xml.push_str(&format!("</bpmn:{tag}>"));
            }
            _ => xml.push_str("/>"),
        }
    }
    for flow in &model.sequence_flows {
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
    xml.push_str("</bpmn:process>");
    if !model.diagram.shapes.is_empty() || !model.diagram.edges.is_empty() {
        xml.push_str(&format!("<bpmndi:BPMNDiagram id=\"Diagram_1\"><bpmndi:BPMNPlane id=\"Plane_1\" bpmnElement=\"{}\">",escaped(&model.process_id)));
        for shape in &model.diagram.shapes {
            xml.push_str(&format!("<bpmndi:BPMNShape id=\"DI_{}\" bpmnElement=\"{}\"><dc:Bounds x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></bpmndi:BPMNShape>",escaped(&shape.element_id),escaped(&shape.element_id),shape.x,shape.y,shape.width,shape.height));
        }
        for edge in &model.diagram.edges {
            xml.push_str(&format!(
                "<bpmndi:BPMNEdge id=\"DI_{}\" bpmnElement=\"{}\">",
                escaped(&edge.sequence_flow_id),
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
}
