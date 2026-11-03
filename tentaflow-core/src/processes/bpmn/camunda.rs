// ===== File: processes/bpmn/camunda.rs — tolerant import of Camunda 7 BPMN XML =====
//
// The strict importer refuses a document at the first thing it does not know.
// A Camunda document is full of such things, so this reader works in three
// steps instead: it reads the XML with the strict reader (same size, entity and
// ID rules, plus a nesting cap), REWRITES the element tree into the TentaFlow
// form while recording one diagnostic per foreign construct, and hands the
// rewritten tree to the strict model builder. Everything the strict reader
// understands therefore keeps one definition.
//
// Rules that shape the rewrite:
// - Nothing foreign is executed or guessed. A construct that cannot be carried
//   over without changing behavior is fatal for its element; one that can be
//   carried over with a loss is a warning; one with no effect is info.
// - Any fatal diagnostic withholds the model. A partial model would silently
//   miss an element, which is worse than an explicit refusal.
// - Expressions go through `expression_translate`; every translation is
//   reported so the person importing sees "before -> after".
// - A document that merely declares the Camunda namespace but holds no Camunda
//   constructs is rewritten to itself and imports exactly like the strict path.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use quick_xml::events::Event;
use quick_xml::Reader;
use tentaflow_protocol::processes::{
    ProcessDiagnostic, ProcessDiagnosticSeverity as Severity, ProcessExecutableProcess,
    ProcessImportTranslation, ProcessModel, ProcessMultiInstanceInput, ProcessMultiInstanceMode,
    ProcessNode, ProcessNodeKind, ProcessRepeatSpec, ServiceImplementation,
};

use super::{
    model_from_tree, node_from_xml, parse_tree_with, Element, EscalationPathError, XmlElementError,
    BPMN, BPMNDI, DC, DI, TF, XSI,
};
use crate::flow_engine::expr;
use crate::processes::expression_translate::{
    read_variables, translate, SourceLanguage, TranslationRefusal,
};
use crate::processes::model::{valid_id, validate_interchange_model, MAX_VARIABLE_KEYS};

pub const CAMUNDA_NAMESPACE: &str = "http://camunda.org/schema/1.0/bpmn";
const CEL_LANGUAGE: &str = "https://cel.dev/spec";
const CEL_SCRIPT_FORMAT: &str = "application/vnd.tentaflow.cel";
/// An unbound step is replaced by a person, so any valid timeout will do.
const UNBOUND_TIMEOUT_SECONDS: &str = "300";
const MAX_DIAGNOSTICS: usize = 1000;
const FLOW_NODES: [&str; 22] = [
    "startEvent",
    "endEvent",
    "intermediateCatchEvent",
    "intermediateThrowEvent",
    "boundaryEvent",
    "userTask",
    "serviceTask",
    "scriptTask",
    "manualTask",
    "sendTask",
    "receiveTask",
    "businessRuleTask",
    "task",
    "callActivity",
    "subProcess",
    "transaction",
    "adHocSubProcess",
    "exclusiveGateway",
    "parallelGateway",
    "inclusiveGateway",
    "eventBasedGateway",
    "complexGateway",
];
const MAX_LITERAL_CHARS: usize = 400;
/// Foreign text can carry any amount of text into a diagnostic message.
const MAX_MESSAGE_CHARS: usize = 1000;
/// An import may look up this many logins; the rest stay unresolved, so a
/// document cannot turn the import into an account scanner.
const MAX_LOGIN_LOOKUPS: usize = 200;

pub struct CamundaImport {
    /// `None` whenever a fatal diagnostic exists.
    pub model: Option<ProcessModel>,
    pub diagnostics: Vec<ProcessDiagnostic>,
    pub translations: Vec<ProcessImportTranslation>,
}

/// Whether the document root declares the Camunda namespace. Only the root tag
/// is read, so the check costs nothing on a hostile document.
pub fn declares_camunda_namespace(xml: &str) -> bool {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Start(start) | Event::Empty(start)) => {
                return start.attributes().flatten().any(|attribute| {
                    let key = attribute.key.as_ref();
                    (key == b"xmlns" || key.starts_with(b"xmlns:"))
                        && attribute.value.as_ref() == CAMUNDA_NAMESPACE.as_bytes()
                });
            }
            Ok(Event::Decl(_) | Event::Comment(_) | Event::Text(_)) => {}
            _ => return false,
        }
    }
}

pub fn import_camunda_xml(xml: &str) -> CamundaImport {
    import_camunda_xml_resolving(xml, &|_| None)
}

/// `resolve_login` maps a Camunda `assignee` login to a TentaFlow user id; the
/// importer itself stays pure and never looks anything up.
pub fn import_camunda_xml_resolving(
    xml: &str,
    resolve_login: &dyn Fn(&str) -> Option<String>,
) -> CamundaImport {
    let mut importer = Importer::new(resolve_login);
    let model = importer.run(xml);
    importer.finish(model)
}

struct RepeatPatch {
    node_id: String,
    base_name: String,
    spec: ProcessRepeatSpec,
}

struct Importer<'a> {
    resolve_login: &'a dyn Fn(&str) -> Option<String>,
    diagnostics: Vec<ProcessDiagnostic>,
    translations: Vec<ProcessImportTranslation>,
    repeats: Vec<RepeatPatch>,
    /// Expression texts read per element id, to find a name an activity uses.
    reads: HashMap<String, BTreeSet<String>>,
    /// Embedded subprocesses whose extension this importer wrote.
    synthesized_subprocesses: HashSet<String>,
    login_lookups: usize,
    fatal: bool,
    fatal_listed: bool,
    fatal_elements: HashSet<String>,
    truncated: bool,
    diagram_detail_dropped: bool,
}

impl<'a> Importer<'a> {
    fn new(resolve_login: &'a dyn Fn(&str) -> Option<String>) -> Self {
        Self {
            resolve_login,
            diagnostics: Vec::new(),
            translations: Vec::new(),
            repeats: Vec::new(),
            reads: HashMap::new(),
            synthesized_subprocesses: HashSet::new(),
            login_lookups: 0,
            fatal: false,
            fatal_listed: false,
            fatal_elements: HashSet::new(),
            truncated: false,
            diagram_detail_dropped: false,
        }
    }

    fn run(&mut self, xml: &str) -> Option<ProcessModel> {
        let mut root = match parse_tree_with(xml, true) {
            Ok(root) => root,
            Err(error) => {
                self.report_error(&error);
                return None;
            }
        };
        if !root.is(BPMN, "definitions") {
            // The strict builder words this refusal.
            return self.build(root);
        }
        let namespace = root.attr("targetNamespace").unwrap_or(TF).to_string();
        self.rewrite_definitions(&mut root);
        self.verify_xsi_types(&root);
        let mut nodes_failed = false;
        for process in root
            .children
            .iter()
            .filter(|child| child.is(BPMN, "process"))
        {
            nodes_failed |= self.check_nodes(process, &namespace);
        }
        if nodes_failed || self.fatal {
            return None;
        }
        self.build(root)
    }

    fn build(&mut self, root: Element) -> Option<ProcessModel> {
        let mut model = match model_from_tree(root) {
            Ok(model) => model,
            Err(error) => {
                self.report_error(&error);
                return None;
            }
        };
        let mut taken = BTreeSet::new();
        collect_names(&model.nodes, &model.variables, &mut taken);
        for process in &model.additional_processes {
            collect_names(&process.nodes, &process.variables, &mut taken);
        }
        let repeats = std::mem::take(&mut self.repeats);
        for mut patch in repeats {
            let final_name = free_name(&patch.base_name, &taken);
            taken.insert(final_name.clone());
            let suffixed = final_name != patch.base_name;
            if let ProcessRepeatSpec::MultiInstance {
                output_collection_variable,
                ..
            } = &mut patch.spec
            {
                *output_collection_variable = final_name.clone();
            }
            let mut applied = patch_repeat(&mut model.nodes, &mut model.variables, &patch);
            for process in &mut model.additional_processes {
                if applied {
                    break;
                }
                applied = patch_process_repeat(process, &patch);
            }
            if applied {
                let why = if suffixed {
                    format!(" ({} was already in use)", patch.base_name)
                } else {
                    String::new()
                };
                self.push(
                    Severity::Warning,
                    "CAMUNDA_MULTI_INSTANCE_OUTPUT_VARIABLE",
                    Some(patch.node_id.clone()),
                    None,
                    format!(
                        "TentaFlow collects the results of a repeat into a variable; \
                         {final_name}{why} was declared for it"
                    ),
                );
            } else {
                self.push(
                    Severity::Fatal,
                    "CAMUNDA_MODEL_INVALID",
                    Some(patch.node_id.clone()),
                    None,
                    format!(
                        "the repeat of {} found no activity to bind to",
                        patch.node_id
                    ),
                );
            }
        }
        let mut propagation = Propagation::default();
        propagate_subprocess_outputs(
            &mut model.nodes,
            &mut model.variables,
            &self.synthesized_subprocesses,
            &mut propagation,
        );
        for process in &mut model.additional_processes {
            propagate_subprocess_outputs(
                &mut process.nodes,
                &mut process.variables,
                &self.synthesized_subprocesses,
                &mut propagation,
            );
        }
        for id in propagation.too_many {
            self.push(
                Severity::Fatal,
                "CAMUNDA_MODEL_INVALID",
                Some(id.clone()),
                None,
                format!(
                    "subprocess {id} writes more variables than a TentaFlow subprocess can hand \
                     in and out ({MAX_VARIABLE_KEYS} at most)"
                ),
            );
        }
        for id in propagation.uncertain {
            self.push(
                Severity::Warning,
                "CAMUNDA_SUBPROCESS_SCOPE",
                Some(id.clone()),
                None,
                format!(
                    "subprocess {id} contains steps whose real implementation is unknown, so the \
                     variables they write cannot be copied out of the subprocess; Camunda would \
                     have propagated them"
                ),
            );
        }
        if self.fatal {
            return None;
        }
        // The same gate the strict importer and the exporter apply, so the model
        // returned here is one the editor can save and export.
        if let Err(error) = validate_interchange_model(&model) {
            self.push(
                Severity::Fatal,
                "CAMUNDA_MODEL_INVALID",
                Some(model.process_id.clone()),
                None,
                format!("the imported process is not a valid TentaFlow model: {error}"),
            );
            return None;
        }
        Some(model)
    }

    fn finish(mut self, model: Option<ProcessModel>) -> CamundaImport {
        if self.diagram_detail_dropped {
            self.push(
                Severity::Info,
                "CAMUNDA_DIAGRAM_DETAIL_IGNORED",
                None,
                None,
                "diagram labels and display markers were not imported; shapes and edges were"
                    .into(),
            );
        }
        if self.truncated {
            self.diagnostics.push(diagnostic(
                Severity::Info,
                "CAMUNDA_DIAGNOSTICS_TRUNCATED",
                None,
                None,
                format!("only the first {MAX_DIAGNOSTICS} diagnostics and translations are listed"),
            ));
        }
        let model = if self.fatal { None } else { model };
        CamundaImport {
            model,
            diagnostics: self.diagnostics,
            translations: self.translations,
        }
    }

    // ----- reporting -----

    fn push(
        &mut self,
        severity: Severity,
        code: &str,
        element_id: Option<String>,
        offset: Option<usize>,
        message: String,
    ) {
        if severity == Severity::Fatal {
            self.fatal = true;
            if let Some(id) = &element_id {
                self.fatal_elements.insert(id.clone());
            }
        }
        if self.diagnostics.len() >= MAX_DIAGNOSTICS {
            self.truncated = true;
            // The refusal must stay visible even when the list is full.
            if severity != Severity::Fatal || self.fatal_listed {
                return;
            }
        }
        self.fatal_listed |= severity == Severity::Fatal;
        self.diagnostics
            .push(diagnostic(severity, code, element_id, offset, message));
    }

    fn report(&mut self, severity: Severity, code: &str, element: &Element, message: String) {
        self.push(
            severity,
            code,
            element.attr("id").map(str::to_string),
            Some(element.offset),
            message,
        );
    }

    /// A strict-reader failure keeps what it knows about its element.
    fn report_error(&mut self, error: &anyhow::Error) {
        let element = error.downcast_ref::<XmlElementError>();
        let path = error.downcast_ref::<EscalationPathError>();
        let mut entry = diagnostic(
            Severity::Fatal,
            "CAMUNDA_INVALID_DOCUMENT",
            element.and_then(|element| element.element_id.clone()),
            element.map(|element| element.offset),
            error.to_string(),
        );
        if let Some(path) = path {
            entry.boundary_id = Some(path.boundary_id.clone());
            entry.flow_id = path.flow_id.clone();
            entry.node_id = path.node_id.clone();
            entry.reason = Some(path.reason);
        }
        self.fatal = true;
        self.fatal_listed = true;
        self.diagnostics.push(entry);
    }

    // ----- expressions -----

    /// Translates one expression and records it. A refusal is returned for the
    /// caller to grade: fatal for a condition, a skipped parameter otherwise.
    fn translate_expression(
        &mut self,
        owner: &Element,
        kind: &str,
        source: &str,
        language: SourceLanguage,
    ) -> Result<String, TranslationRefusal> {
        let element_id = owner.attr("id").unwrap_or_default().to_string();
        let translation = translate(source, language)?;
        // The collection expression is evaluated outside the repeated activity,
        // so its own reads are not reads of the element variable.
        if kind != "multi_instance_collection" {
            if let Some(names) = read_variables(source, language) {
                self.reads
                    .entry(element_id.clone())
                    .or_default()
                    .extend(names);
            }
        }
        let mut codes = Vec::new();
        for warning in &translation.warnings {
            codes.push(warning.code.to_string());
            self.report(
                Severity::Warning,
                "CAMUNDA_EXPRESSION_WARNING",
                owner,
                format!("{}: {}", warning.code, warning.message),
            );
        }
        self.record_translation(ProcessImportTranslation {
            element_id,
            kind: kind.into(),
            original: source.into(),
            translated: translation.cel.clone(),
            warnings: codes,
        });
        Ok(translation.cel)
    }

    /// Translations are capped like diagnostics, for the same reason.
    fn record_translation(&mut self, translation: ProcessImportTranslation) {
        if self.translations.len() >= MAX_DIAGNOSTICS {
            self.truncated = true;
            return;
        }
        self.translations.push(translation);
    }

    fn refusal(
        &mut self,
        severity: Severity,
        owner: &Element,
        what: &str,
        refusal: &TranslationRefusal,
    ) {
        self.report(
            severity,
            "CAMUNDA_EXPRESSION_REFUSED",
            owner,
            format!("{what}: {refusal}"),
        );
    }

    // ----- document -----

    fn rewrite_definitions(&mut self, root: &mut Element) {
        self.drop_unprefixed(
            root,
            &[
                "exporter",
                "exporterVersion",
                "expressionLanguage",
                "typeLanguage",
            ],
        );
        self.rewrite_foreign_attributes(root);
        let mut kept = Vec::new();
        for mut child in std::mem::take(&mut root.children) {
            if child.ns == BPMN && child.local == "process" {
                self.rewrite_process(&mut child);
            } else if child.ns == BPMNDI && child.local == "BPMNDiagram" {
                self.rewrite_diagram(&mut child);
            } else if child.ns == BPMN
                && matches!(
                    child.local.as_str(),
                    "itemDefinition" | "resource" | "import" | "extension" | "relationship"
                )
            {
                self.report(
                    Severity::Info,
                    "CAMUNDA_ELEMENT_IGNORED",
                    &child,
                    format!("top-level {} has no TentaFlow counterpart", child.local),
                );
                continue;
            } else if child.ns != BPMN {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_FOREIGN_ELEMENT",
                    &child,
                    format!("element {{{}}}{} was not imported", child.ns, child.local),
                );
                continue;
            }
            kept.push(child);
        }
        root.children = kept;
    }

    fn rewrite_process(&mut self, process: &mut Element) {
        self.drop_unprefixed(
            process,
            &["processType", "isClosed", "definitionalCollaborationRef"],
        );
        self.rewrite_foreign_attributes(process);
        self.strip_annotations(process, false);
        let mut ext = self.take_extension(process);
        if let Some(extension) = ext.as_mut() {
            let _ = self.camunda_extension_children(process, extension);
            self.finish_extension(process, ext);
        }
        self.rewrite_graph(process);
    }

    /// The flow nodes and flows of a process or an embedded subprocess.
    fn rewrite_graph(&mut self, container: &mut Element) {
        let mut children = std::mem::take(&mut container.children);
        for child in &mut children {
            if child.ns != BPMN {
                continue;
            }
            match child.local.as_str() {
                "sequenceFlow" => self.rewrite_flow(child),
                local if FLOW_NODES.contains(&local) => self.rewrite_node(child),
                _ => {}
            }
        }
        for gateway in children
            .iter()
            .filter(|child| child.is(BPMN, "exclusiveGateway"))
        {
            let id = gateway.attr("id").unwrap_or_default();
            // An outgoing flow without a condition that is not the default
            // evaluates as true in TentaFlow, so it takes part in the race too.
            let default = gateway.attr("default");
            let competing = children
                .iter()
                .filter(|flow| {
                    flow.is(BPMN, "sequenceFlow")
                        && flow.attr("sourceRef") == Some(id)
                        && flow.attr("id") != default
                })
                .count();
            if competing >= 2 {
                let message = "TentaFlow raises an incident on the first condition evaluation \
                    error and when more than one condition is true, while Camunda takes the \
                    first true flow in document order; this gateway can behave differently"
                    .to_string();
                self.report(
                    Severity::Warning,
                    "CAMUNDA_GATEWAY_EVAL_SEMANTICS",
                    gateway,
                    message,
                );
            }
        }
        container.children = children;
    }

    // ----- sequence flows -----

    fn rewrite_flow(&mut self, flow: &mut Element) {
        self.drop_unprefixed(flow, &["name", "isImmediate"]);
        self.rewrite_foreign_attributes(flow);
        self.drop_xsi_type(flow);
        self.strip_annotations(flow, false);
        let mut ext = self.take_extension(flow);
        if let Some(extension) = ext.as_mut() {
            let _ = self.camunda_extension_children(flow, extension);
            self.finish_extension(flow, ext);
        }
        let Some(position) = flow
            .children
            .iter()
            .position(|c| c.is(BPMN, "conditionExpression"))
        else {
            return;
        };
        let mut condition = flow.children.remove(position);
        self.rewrite_condition(flow, &mut condition);
        flow.children.insert(position, condition);
    }

    fn rewrite_condition(&mut self, flow: &Element, condition: &mut Element) {
        let type_name = condition
            .attrs
            .get(&(XSI.to_string(), "type".to_string()))
            .cloned();
        if let Some(type_name) = &type_name {
            let known = [
                format!("{{{BPMN}}}tFormalExpression"),
                format!("{{{BPMN}}}tExpression"),
            ];
            if !known.contains(type_name) {
                self.report(
                    Severity::Fatal,
                    "CAMUNDA_UNSUPPORTED_XSI_TYPE",
                    flow,
                    format!("condition type {type_name} is not a BPMN expression"),
                );
                return;
            }
        }
        if condition
            .attrs
            .keys()
            .any(|(ns, name)| ns == CAMUNDA_NAMESPACE && name == "resource")
        {
            self.report(
                Severity::Fatal,
                "CAMUNDA_CONDITION_RESOURCE",
                flow,
                "a condition read from an external resource cannot be imported".into(),
            );
            return;
        }
        // Every foreign attribute of the expression is dropped below; the ones
        // that matter are decided here.
        self.rewrite_foreign_attributes(condition);
        if !condition.children.is_empty() {
            self.report(
                Severity::Fatal,
                "CAMUNDA_INVALID_CONDITION",
                flow,
                "a condition expression must be text".into(),
            );
            return;
        }
        let source = condition.text.clone();
        let language = condition.attr("language").map(str::to_string);
        let mode = match language.as_deref() {
            Some(CEL_LANGUAGE) => return,
            None if !has_wrapper(&source) => {
                self.report(
                    Severity::Fatal,
                    "CAMUNDA_CONDITION_UNTYPED",
                    flow,
                    "a condition without a language and without ${...} is a plain string in \
                     Camunda, not a boolean test; it is not guessed to be CEL"
                        .into(),
                );
                return;
            }
            None => SourceLanguage::Juel,
            Some(other) => match other.to_ascii_lowercase().as_str() {
                "juel" | "el" | "http://www.omg.org/spec/el/20100524" => SourceLanguage::Juel,
                "feel" | "http://www.omg.org/spec/feel/20140401" => SourceLanguage::Feel,
                _ => {
                    self.report(
                        Severity::Fatal,
                        "CAMUNDA_CONDITION_LANGUAGE",
                        flow,
                        format!(
                            "condition language {other} is never executed; only JUEL and FEEL \
                             conditions are translated"
                        ),
                    );
                    return;
                }
            },
        };
        match self.translate_expression(flow, "condition", &source, mode) {
            Ok(cel) => {
                condition.text = cel;
                condition
                    .attrs
                    .insert((String::new(), "language".into()), CEL_LANGUAGE.into());
            }
            Err(refusal) => self.refusal(Severity::Fatal, flow, "condition", &refusal),
        }
    }

    // ----- flow nodes -----

    fn rewrite_node(&mut self, node: &mut Element) {
        let local = node.local.clone();
        self.drop_unprefixed(
            node,
            &[
                "isForCompensation",
                "startQuantity",
                "completionQuantity",
                "parallelMultiple",
            ],
        );
        if matches!(local.as_str(), "serviceTask") {
            self.drop_unprefixed(node, &["implementation"]);
        }
        self.drop_xsi_type(node);
        let keep_documentation = local == "manualTask";
        self.strip_annotations(node, keep_documentation);

        let mut ext = self.take_extension(node);
        let mut parameters = Parameters::default();
        if let Some(extension) = ext.as_mut() {
            parameters = self.camunda_extension_children(node, extension);
        }
        let repeat_bound = ext
            .as_ref()
            .is_some_and(|extension| extension.children.iter().any(|c| c.is(TF, "repeat")));

        match local.as_str() {
            "serviceTask" => self.rewrite_service_task(node, &mut ext, parameters),
            "scriptTask" => self.rewrite_script_task(node, &mut ext, parameters),
            "userTask" => self.rewrite_user_task(node, &mut ext, parameters),
            _ => self.skip_parameters(node, parameters, "this element has no mapping"),
        }
        self.rewrite_foreign_attributes(node);
        self.finish_extension(node, ext);
        if local == "subProcess" {
            self.prepare_subprocess(node);
            self.rewrite_graph(node);
        }
        // Last, so the expressions of a subprocess body are already read when
        // the element variable is looked up in them.
        if !repeat_bound {
            self.rewrite_multi_instance(node);
        }
    }

    /// The strict reader requires the TentaFlow subprocess extension, which a
    /// foreign document cannot have.
    fn prepare_subprocess(&mut self, node: &mut Element) {
        let offset = node.offset;
        if let Some(id) = node.attr("id") {
            if !node.children.iter().any(|c| {
                c.is(BPMN, "extensionElements") && c.children.iter().any(|e| e.is(TF, "subProcess"))
            }) {
                self.synthesized_subprocesses.insert(id.to_string());
            }
        }
        let position = node
            .children
            .iter()
            .position(|c| c.is(BPMN, "extensionElements"));
        let extension = match position {
            Some(position) => &mut node.children[position],
            None => {
                let at = leading_documentation(&node.children);
                node.children.insert(
                    at,
                    element(BPMN, "extensionElements", &[], Vec::new(), "", offset),
                );
                &mut node.children[at]
            }
        };
        if !extension.children.iter().any(|c| c.is(TF, "subProcess")) {
            let config = element(
                TF,
                "subProcess",
                &[],
                vec![element(TF, "variables", &[], Vec::new(), "{}", offset)],
                "",
                offset,
            );
            extension.children.insert(0, config);
        }
    }

    fn rewrite_service_task(
        &mut self,
        node: &mut Element,
        ext: &mut Option<Element>,
        parameters: Parameters,
    ) {
        let class = take_camunda(node, "class");
        let delegate = take_camunda(node, "delegateExpression");
        let expression = take_camunda(node, "expression");
        let kind = take_camunda(node, "type");
        let topic = take_camunda(node, "topic");
        let result_variable = take_camunda(node, "resultVariable");
        let native = ext
            .as_ref()
            .is_some_and(|extension| extension.children.iter().any(|c| c.is(TF, "service")));
        if native {
            for (name, value) in [
                ("class", &class),
                ("delegateExpression", &delegate),
                ("expression", &expression),
                ("type", &kind),
                ("topic", &topic),
                ("resultVariable", &result_variable),
            ] {
                if let Some(value) = value {
                    self.native_attribute_ignored(node, name, value);
                }
            }
            self.skip_parameters(
                node,
                parameters,
                "the step already has a TentaFlow implementation",
            );
            return;
        }
        let origin = if let Some(class) = class {
            format!("class:{class}")
        } else if let Some(delegate) = delegate {
            format!("delegateExpression:{delegate}")
        } else if let Some(expression) = expression {
            format!("expression:{expression}")
        } else if let Some(connector) = parameters.connector.as_deref() {
            format!("connector:{connector}")
        } else if kind.as_deref() == Some("external") {
            format!("external:{}", topic.unwrap_or_default())
        } else if let Some(kind) = kind {
            format!("type:{kind}")
        } else {
            "unspecified".to_string()
        };
        let origin = bounded_origin(&origin);
        if origin == "unspecified" {
            self.report(
                Severity::Warning,
                "CAMUNDA_SERVICE_NO_IMPLEMENTATION",
                node,
                "the service task names no implementation; it was imported as a placeholder".into(),
            );
        } else {
            self.report(
                Severity::Warning,
                "CAMUNDA_SERVICE_UNBOUND",
                node,
                format!(
                    "{origin} has no TentaFlow counterpart; the step is an unbound placeholder \
                     that blocks publication until a flow, tool, block or agent is chosen"
                ),
            );
        }
        let mut output = parameters.output;
        if let Some(variable) = result_variable {
            if valid_id(&variable) && !output.contains_key(&variable) {
                output.insert(variable, "outputs".into());
            } else {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_RESULT_VARIABLE_SKIPPED",
                    node,
                    format!("result variable {variable} cannot be a TentaFlow variable name"),
                );
            }
        }
        let offset = node.offset;
        let mut children = Vec::new();
        if !parameters.input.is_empty() {
            children.push(json_element("inputMapping", &parameters.input, offset));
        }
        if !output.is_empty() {
            children.push(json_element("outputMapping", &output, offset));
        }
        let service = element(
            TF,
            "service",
            &[
                ("implementation", "unbound"),
                ("origin", &origin),
                ("timeoutSeconds", UNBOUND_TIMEOUT_SECONDS),
                ("verification", "human"),
            ],
            children,
            "",
            offset,
        );
        ext.get_or_insert_with(|| element(BPMN, "extensionElements", &[], Vec::new(), "", offset))
            .children
            .insert(0, service);
    }

    fn rewrite_script_task(
        &mut self,
        node: &mut Element,
        ext: &mut Option<Element>,
        parameters: Parameters,
    ) {
        let format = node.attr("scriptFormat").map(str::to_string);
        let result_variable = take_camunda(node, "resultVariable");
        let resource = take_camunda(node, "resource");
        self.skip_parameters(
            node,
            parameters,
            "a script task has no input or output mapping",
        );
        if format.as_deref() == Some(CEL_SCRIPT_FORMAT) {
            for (name, value) in [
                ("resource", &resource),
                ("resultVariable", &result_variable),
            ] {
                if let Some(value) = value {
                    self.native_attribute_ignored(node, name, value);
                }
            }
            return;
        }
        if resource.is_some() {
            self.report(
                Severity::Fatal,
                "CAMUNDA_SCRIPT_UNSUPPORTED",
                node,
                "a script read from an external resource is never executed".into(),
            );
            return;
        }
        let language = format.as_deref().unwrap_or_default().to_ascii_lowercase();
        let mode = match language.as_str() {
            "juel" => SourceLanguage::Juel,
            "feel" => SourceLanguage::Feel,
            _ => {
                self.report(
                    Severity::Fatal,
                    "CAMUNDA_SCRIPT_UNSUPPORTED",
                    node,
                    format!(
                        "a {} script is never executed; only a JUEL or FEEL script that is a \
                         pure expression is imported",
                        if language.is_empty() {
                            "script of unknown language"
                        } else {
                            &language
                        }
                    ),
                );
                return;
            }
        };
        let Some(body) = node.children.iter().position(|c| c.is(BPMN, "script")) else {
            self.report(
                Severity::Fatal,
                "CAMUNDA_SCRIPT_UNSUPPORTED",
                node,
                "the script task has no script body".into(),
            );
            return;
        };
        let source = node.children[body].text.clone();
        let cel = match self.translate_expression(node, "script", &source, mode) {
            Ok(cel) => cel,
            Err(refusal) => {
                self.refusal(Severity::Fatal, node, "script", &refusal);
                return;
            }
        };
        // A script task runs the bounded script profile, which is narrower than
        // the expression language the translator targets.
        if let Err(error) = expr::validate_script_profile(&cel) {
            self.report(
                Severity::Fatal,
                "CAMUNDA_SCRIPT_UNSUPPORTED",
                node,
                format!("the translated script is outside the TentaFlow script profile: {error}"),
            );
            return;
        }
        let offset = node.offset;
        node.children[body].text = cel;
        node.attrs.insert(
            (String::new(), "scriptFormat".into()),
            CEL_SCRIPT_FORMAT.into(),
        );
        // The script body must be the last child of its element.
        let body = node.children.remove(body);
        node.children.push(body);
        if let Some(variable) = result_variable {
            if !valid_id(&variable) {
                self.report(
                    Severity::Fatal,
                    "CAMUNDA_SCRIPT_UNSUPPORTED",
                    node,
                    format!("result variable {variable} cannot be a TentaFlow variable name"),
                );
                return;
            }
            let mapping = BTreeMap::from([(variable, "outputs".to_string())]);
            let config = element(
                TF,
                "scriptTask",
                &[],
                vec![json_element("outputMapping", &mapping, offset)],
                "",
                offset,
            );
            ext.get_or_insert_with(|| {
                element(BPMN, "extensionElements", &[], Vec::new(), "", offset)
            })
            .children
            .insert(0, config);
        }
    }

    fn rewrite_user_task(
        &mut self,
        node: &mut Element,
        ext: &mut Option<Element>,
        parameters: Parameters,
    ) {
        let assignee = take_camunda(node, "assignee");
        let native = ext
            .as_ref()
            .is_some_and(|extension| extension.children.iter().any(|c| c.is(TF, "user")));
        self.skip_parameters_input(node, &parameters);
        if native {
            if let Some(login) = &assignee {
                self.native_attribute_ignored(node, "assignee", login);
            }
            if !parameters.output.is_empty() {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_PARAMETERS_SKIPPED",
                    node,
                    "output parameters were not imported: the step already has a TentaFlow \
                     output mapping"
                        .into(),
                );
            }
            return;
        }
        let mut user_id = None;
        if let Some(login) = assignee {
            let lookup = !login.contains("${")
                && !login.contains("#{")
                && self.login_lookups < MAX_LOGIN_LOOKUPS;
            if lookup {
                self.login_lookups += 1;
            }
            let resolved = lookup.then(|| (self.resolve_login)(&login)).flatten();
            match resolved {
                Some(id) => user_id = Some(id),
                None => self.report(
                    Severity::Warning,
                    "CAMUNDA_ASSIGNEE_UNRESOLVED",
                    node,
                    format!(
                        "assignee {login} is not a TentaFlow user of this organization; the task \
                         is left unassigned"
                    ),
                ),
            }
        }
        if user_id.is_none() && parameters.output.is_empty() {
            return;
        }
        let offset = node.offset;
        let mut attrs = Vec::new();
        if let Some(id) = user_id.as_deref() {
            attrs.push(("assigneeUserId", id));
        }
        let mut children = Vec::new();
        if !parameters.output.is_empty() {
            children.push(json_element("outputMapping", &parameters.output, offset));
        }
        let user = element(TF, "user", &attrs, children, "", offset);
        ext.get_or_insert_with(|| element(BPMN, "extensionElements", &[], Vec::new(), "", offset))
            .children
            .insert(0, user);
    }

    fn native_attribute_ignored(&mut self, node: &Element, name: &str, value: &str) {
        self.report(
            Severity::Warning,
            "CAMUNDA_NATIVE_STEP_ATTRIBUTE_IGNORED",
            node,
            format!(
                "camunda:{name}=\"{}\" was not imported: the step already carries its \
                 TentaFlow definition",
                clip(value)
            ),
        );
    }

    fn skip_parameters(&mut self, node: &Element, parameters: Parameters, reason: &str) {
        if !parameters.input.is_empty() || !parameters.output.is_empty() {
            self.report(
                Severity::Warning,
                "CAMUNDA_PARAMETERS_SKIPPED",
                node,
                format!("input/output parameters were not imported: {reason}"),
            );
        }
    }

    fn skip_parameters_input(&mut self, node: &Element, parameters: &Parameters) {
        if !parameters.input.is_empty() {
            self.report(
                Severity::Warning,
                "CAMUNDA_PARAMETERS_SKIPPED",
                node,
                "input parameters were not imported: a user task has no input mapping".into(),
            );
        }
    }

    // ----- multi-instance -----

    fn rewrite_multi_instance(&mut self, node: &mut Element) {
        let Some(position) = node
            .children
            .iter()
            .position(|c| c.is(BPMN, "multiInstanceLoopCharacteristics"))
        else {
            return;
        };
        let mut mi = node.children.remove(position);
        let collection = take_camunda(&mut mi, "collection");
        let element_variable = take_camunda(&mut mi, "elementVariable");
        let sequential =
            mi.attr("isSequential") == Some("true") || mi.attr("isSequential") == Some("1");
        self.rewrite_foreign_attributes(&mut mi);
        let id = node.attr("id").unwrap_or_default().to_string();

        let mut cardinality = None;
        let mut invalid = false;
        for child in &mut mi.children {
            if child.is(BPMN, "extensionElements") {
                let _ = self.camunda_extension_children(node, child);
            } else if matches!(
                child.local.as_str(),
                "loopDataInputRef" | "inputDataItem" | "loopDataOutputRef" | "outputDataItem"
            ) {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_MULTI_INSTANCE_DATA_SKIPPED",
                    node,
                    format!(
                        "multi-instance data binding {} was not imported",
                        child.local
                    ),
                );
            } else if child.is(BPMN, "loopCardinality") {
                cardinality = Some(child.text.trim().to_string());
            } else if child.is(BPMN, "completionCondition") {
                invalid = true;
                self.report(
                    Severity::Fatal,
                    "CAMUNDA_MULTI_INSTANCE_COMPLETION",
                    node,
                    "a completion condition ends the activity early; TentaFlow runs every \
                     instance to the end, so the behavior would change"
                        .into(),
                );
            }
        }
        if invalid {
            return;
        }
        if collection.is_some() && cardinality.is_some() {
            self.report(
                Severity::Fatal,
                "CAMUNDA_MULTI_INSTANCE_INPUT",
                node,
                "the activity names both a collection and a loop cardinality; which one wins is \
                 not carried over"
                    .into(),
            );
            return;
        }
        let input = if let Some(collection) = collection {
            let source = if has_wrapper(&collection) {
                collection.clone()
            } else if is_plain_name(&collection) {
                format!("${{{collection}}}")
            } else {
                self.report(
                    Severity::Fatal,
                    "CAMUNDA_MULTI_INSTANCE_COLLECTION",
                    node,
                    format!("collection {collection} is neither a variable nor an expression"),
                );
                return;
            };
            match self.translate_expression(
                node,
                "multi_instance_collection",
                &source,
                SourceLanguage::Juel,
            ) {
                Ok(cel) => ProcessMultiInstanceInput::CollectionExpression { expression: cel },
                Err(refusal) => {
                    self.refusal(Severity::Fatal, node, "multi-instance collection", &refusal);
                    return;
                }
            }
        } else if let Some(text) = cardinality {
            match text.parse::<u8>() {
                Ok(count) if count <= 16 && text.bytes().all(|b| b.is_ascii_digit()) => {
                    ProcessMultiInstanceInput::Cardinality { count }
                }
                _ => {
                    self.report(
                        Severity::Fatal,
                        "CAMUNDA_MULTI_INSTANCE_CARDINALITY",
                        node,
                        format!(
                            "loop cardinality {text} is not a literal count up to 16, which is \
                             all a TentaFlow repeat can express"
                        ),
                    );
                    return;
                }
            }
        } else {
            self.report(
                Severity::Fatal,
                "CAMUNDA_MULTI_INSTANCE_INPUT",
                node,
                "the multi-instance activity has neither camunda:collection nor a loop \
                 cardinality"
                    .into(),
            );
            return;
        };
        if let Some(variable) = element_variable {
            let used = self.uses_name(node, &variable);
            self.report(
                if used {
                    Severity::Fatal
                } else {
                    Severity::Warning
                },
                "CAMUNDA_MULTI_INSTANCE_ELEMENT_VARIABLE",
                node,
                if used {
                    format!(
                        "element variable {variable} is read by an expression of this activity, \
                         but TentaFlow exposes the current item as repeat.item, so the \
                         expression would read another variable"
                    )
                } else {
                    format!(
                        "element variable {variable} is not bound; TentaFlow exposes the \
                         current item as repeat.item"
                    )
                },
            );
            if used {
                return;
            }
        }
        let output_collection_variable = results_variable(&id);
        self.repeats.push(RepeatPatch {
            node_id: id,
            base_name: output_collection_variable.clone(),
            spec: ProcessRepeatSpec::MultiInstance {
                mode: if sequential {
                    ProcessMultiInstanceMode::Sequential
                } else {
                    ProcessMultiInstanceMode::Parallel
                },
                input,
                output_collection_variable,
            },
        });
    }

    /// Whether an expression read inside `node` (or a step nested in it) names
    /// the variable as a free identifier.
    fn uses_name(&self, node: &Element, name: &str) -> bool {
        let mut ids = Vec::new();
        collect_ids(node, &mut ids);
        ids.iter()
            .any(|id| self.reads.get(id).is_some_and(|names| names.contains(name)))
    }

    // ----- extension elements and attributes -----

    fn take_extension(&mut self, element: &mut Element) -> Option<Element> {
        let position = element
            .children
            .iter()
            .position(|c| c.is(BPMN, "extensionElements"))?;
        Some(element.children.remove(position))
    }

    /// Puts the extension back unless nothing is left in it.
    fn finish_extension(&mut self, owner: &mut Element, extension: Option<Element>) {
        if let Some(extension) = extension {
            if !extension.children.is_empty() {
                // The strict reader wants documentation before the extension.
                let at = leading_documentation(&owner.children);
                owner.children.insert(at, extension);
            }
        }
    }

    /// Consumes the Camunda children of an `extensionElements`, leaving the
    /// TentaFlow ones, and returns the input/output parameters found.
    fn camunda_extension_children(
        &mut self,
        owner: &Element,
        extension: &mut Element,
    ) -> Parameters {
        let mut parameters = Parameters::default();
        let mut kept = Vec::new();
        for child in std::mem::take(&mut extension.children) {
            if child.ns == TF {
                kept.push(child);
                continue;
            }
            if child.ns != CAMUNDA_NAMESPACE {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_FOREIGN_ELEMENT",
                    owner,
                    format!("extension {{{}}}{} was not imported", child.ns, child.local),
                );
                continue;
            }
            match child.local.as_str() {
                "inputOutput" => self.read_parameters(owner, &child, &mut parameters),
                "executionListener" | "taskListener" => self.report(
                    Severity::Warning,
                    "CAMUNDA_LISTENER_SKIPPED",
                    owner,
                    format!(
                        "{} {} was not imported; it may have side effects the process relied on",
                        child.local,
                        describe_listener(&child)
                    ),
                ),
                "failedJobRetryTimeCycle" => self.report(
                    Severity::Warning,
                    "CAMUNDA_RETRY_CYCLE_SKIPPED",
                    owner,
                    format!(
                        "retry cycle {} was not imported; per-activity retries do not exist yet",
                        child.text.trim()
                    ),
                ),
                "formData" => self.report(
                    Severity::Warning,
                    "CAMUNDA_FORM_SKIPPED",
                    owner,
                    "the form definition was not imported".into(),
                ),
                "properties" => self.report(
                    Severity::Info,
                    "CAMUNDA_PROPERTIES_IGNORED",
                    owner,
                    "extension properties were not imported".into(),
                ),
                "field" => self.report(
                    Severity::Info,
                    "CAMUNDA_FIELD_IGNORED",
                    owner,
                    format!(
                        "field injection {} belongs to the foreign implementation",
                        child.attr("name").unwrap_or("?")
                    ),
                ),
                "connector" => {
                    let connector = child
                        .children
                        .iter()
                        .find(|c| c.is(CAMUNDA_NAMESPACE, "connectorId"))
                        .map(|c| c.text.trim().to_string())
                        .unwrap_or_default();
                    parameters.connector = Some(connector.clone());
                    if child
                        .children
                        .iter()
                        .any(|inner| inner.is(CAMUNDA_NAMESPACE, "inputOutput"))
                    {
                        self.report(
                            Severity::Warning,
                            "CAMUNDA_CONNECTOR_PARAMETERS_SKIPPED",
                            owner,
                            format!("the parameters of connector {connector} belong to it and were not imported"),
                        );
                    }
                    self.report(
                        Severity::Warning,
                        "CAMUNDA_CONNECTOR_SKIPPED",
                        owner,
                        format!("connector {connector} was not imported"),
                    );
                }
                "in" | "out" => self.report(
                    Severity::Warning,
                    "CAMUNDA_CALL_MAPPING_SKIPPED",
                    owner,
                    "a call activity variable mapping was not imported".into(),
                ),
                other => self.report(
                    Severity::Warning,
                    "CAMUNDA_UNKNOWN_ELEMENT",
                    owner,
                    format!("camunda:{other} was not imported"),
                ),
            }
        }
        extension.children = kept;
        parameters
    }

    fn read_parameters(&mut self, owner: &Element, io: &Element, parameters: &mut Parameters) {
        for parameter in &io.children {
            let output = parameter.is(CAMUNDA_NAMESPACE, "outputParameter");
            if !output && !parameter.is(CAMUNDA_NAMESPACE, "inputParameter") {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_UNKNOWN_ELEMENT",
                    owner,
                    format!("camunda:{} was not imported", parameter.local),
                );
                continue;
            }
            let Some(name) = parameter.attr("name").map(str::to_string) else {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_PARAMETER_SKIPPED",
                    owner,
                    "a parameter without a name was not imported".into(),
                );
                continue;
            };
            if !valid_id(&name) {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_PARAMETER_SKIPPED",
                    owner,
                    format!("parameter {name} is not a valid TentaFlow variable name"),
                );
                continue;
            }
            if let Some(structure) = parameter.children.first() {
                let (code, what) = if structure.is(CAMUNDA_NAMESPACE, "script") {
                    ("CAMUNDA_PARAMETER_SCRIPT_SKIPPED", "a script")
                } else {
                    ("CAMUNDA_PARAMETER_STRUCTURE_SKIPPED", "a list or map")
                };
                self.report(
                    Severity::Warning,
                    code,
                    owner,
                    format!("parameter {name} is {what}, which is not imported"),
                );
                continue;
            }
            let kind = if output {
                "output_parameter"
            } else {
                "input_parameter"
            };
            let target = if output {
                &mut parameters.output
            } else {
                &mut parameters.input
            };
            if target.contains_key(&name) {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_PARAMETER_SKIPPED",
                    owner,
                    format!("parameter {name} is defined twice; the first definition is kept"),
                );
                continue;
            }
            let raw = parameter.text.clone();
            if raw.trim().is_empty() {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_PARAMETER_SKIPPED",
                    owner,
                    format!("parameter {name} has no value"),
                );
                continue;
            }
            let cel = if has_wrapper(&raw) {
                match self.translate_expression(owner, kind, &raw, SourceLanguage::Juel) {
                    Ok(cel) => cel,
                    Err(refusal) => {
                        self.refusal(
                            Severity::Warning,
                            owner,
                            &format!("parameter {name} was not imported"),
                            &refusal,
                        );
                        continue;
                    }
                }
            } else if raw.chars().count() > MAX_LITERAL_CHARS {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_PARAMETER_SKIPPED",
                    owner,
                    format!("parameter {name} is longer than {MAX_LITERAL_CHARS} characters"),
                );
                continue;
            } else {
                let cel = cel_string(&raw);
                self.record_translation(ProcessImportTranslation {
                    element_id: owner.attr("id").unwrap_or_default().to_string(),
                    kind: kind.into(),
                    original: raw.clone(),
                    translated: cel.clone(),
                    warnings: Vec::new(),
                });
                cel
            };
            if output {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_OUTPUT_PARAMETER_SCOPE",
                    owner,
                    format!(
                        "output parameter {name} is evaluated against the step result \
                         (outputs) and process variables, not Camunda's task-local variables"
                    ),
                );
            }
            target.insert(name, cel);
        }
    }

    /// Removes the named unprefixed attributes, reporting each as ignored.
    fn drop_unprefixed(&mut self, element: &mut Element, names: &[&str]) {
        for name in names {
            if let Some(value) = element.attrs.remove(&(String::new(), (*name).to_string())) {
                // Only a value that differs from the BPMN default carries meaning.
                let lost = matches!(
                    (*name, value.as_str()),
                    ("isForCompensation", v) | ("parallelMultiple", v) if v != "false"
                ) || matches!(
                    (*name, value.as_str()),
                    ("startQuantity", v) | ("completionQuantity", v) if v != "1"
                );
                self.report(
                    if lost {
                        Severity::Warning
                    } else {
                        Severity::Info
                    },
                    if lost {
                        "CAMUNDA_ATTRIBUTE_LOST"
                    } else {
                        "CAMUNDA_ATTRIBUTE_IGNORED"
                    },
                    element,
                    format!(
                        "attribute {name}=\"{}\" has no TentaFlow counterpart",
                        clip(&value)
                    ),
                );
            }
        }
    }

    /// Reports and removes every attribute in a foreign namespace.
    fn rewrite_foreign_attributes(&mut self, element: &mut Element) {
        let mut foreign: Vec<(String, String)> = element
            .attrs
            .keys()
            .filter(|(ns, _)| !ns.is_empty() && ns != XSI)
            .cloned()
            .collect();
        foreign.sort();
        for key in foreign {
            let value = element.attrs.remove(&key).unwrap_or_default();
            let (ns, name) = key;
            let (severity, code, what) = if ns == CAMUNDA_NAMESPACE {
                match name.as_str() {
                    "asyncBefore" | "asyncAfter" | "exclusive" => (
                        Severity::Info,
                        "CAMUNDA_ASYNC_IGNORED",
                        "job execution settings do not apply",
                    ),
                    "jobPriority" | "taskPriority" => (
                        Severity::Info,
                        "CAMUNDA_PRIORITY_IGNORED",
                        "job priorities do not apply",
                    ),
                    "historyTimeToLive" => (
                        Severity::Info,
                        "CAMUNDA_HISTORY_TTL_IGNORED",
                        "history cleanup is configured elsewhere",
                    ),
                    "versionTag" | "isStartableInTasklist" => (
                        Severity::Info,
                        "CAMUNDA_ATTRIBUTE_IGNORED",
                        "it has no TentaFlow counterpart",
                    ),
                    "candidateUsers"
                    | "candidateGroups"
                    | "candidateStarterUsers"
                    | "candidateStarterGroups" => (
                        Severity::Warning,
                        "CAMUNDA_CANDIDATES_SKIPPED",
                        "candidate assignment is not imported",
                    ),
                    "formKey" | "formRef" | "formRefBinding" | "formRefVersion" => (
                        Severity::Warning,
                        "CAMUNDA_FORM_SKIPPED",
                        "forms are not imported",
                    ),
                    "dueDate" | "followUpDate" | "priority" => (
                        Severity::Warning,
                        "CAMUNDA_USER_TASK_ATTRIBUTE_SKIPPED",
                        "task deadlines and priorities are not imported",
                    ),
                    _ => (
                        Severity::Warning,
                        "CAMUNDA_UNKNOWN_ATTRIBUTE",
                        "it is not understood and was not imported",
                    ),
                }
            } else {
                (
                    Severity::Info,
                    "CAMUNDA_FOREIGN_ATTRIBUTE",
                    "it belongs to a foreign namespace",
                )
            };
            self.report(
                severity,
                code,
                element,
                format!("attribute {name}=\"{}\": {what}", clip(&value)),
            );
        }
    }

    /// `xsi:type` is meaningful only on expressions, which handle it themselves.
    fn drop_xsi_type(&mut self, element: &mut Element) {
        if let Some(value) = element.attrs.remove(&(XSI.to_string(), "type".to_string())) {
            self.report(
                Severity::Warning,
                "CAMUNDA_XSI_TYPE_IGNORED",
                element,
                format!("xsi:type {value} is not supported on {}", element.local),
            );
        }
    }

    /// Drops the redundant `incoming`/`outgoing` references (the flows carry the
    /// same information) and documentation, which TentaFlow keeps only on manual
    /// tasks, and removes children from foreign namespaces.
    fn strip_annotations(&mut self, element: &mut Element, keep_documentation: bool) {
        let mut kept = Vec::new();
        for child in std::mem::take(&mut element.children) {
            if child.ns == BPMN && matches!(child.local.as_str(), "incoming" | "outgoing") {
                continue;
            }
            if child.ns == BPMN && child.local == "documentation" && !keep_documentation {
                self.report(
                    Severity::Info,
                    "CAMUNDA_DOCUMENTATION_IGNORED",
                    element,
                    "documentation was not imported".into(),
                );
                continue;
            }
            if child.ns != BPMN {
                self.report(
                    Severity::Warning,
                    "CAMUNDA_FOREIGN_ELEMENT",
                    element,
                    format!("element {{{}}}{} was not imported", child.ns, child.local),
                );
                continue;
            }
            kept.push(child);
        }
        element.children = kept;
    }

    /// Every `xsi:type` left after the rewrite must be a BPMN formal expression;
    /// the tolerant reader kept the others so they could be judged here.
    fn verify_xsi_types(&mut self, root: &Element) {
        let known = [
            format!("{{{BPMN}}}tFormalExpression"),
            format!("{{{BPMN}}}tExpression"),
        ];
        let mut pending = vec![(root, root)];
        while let Some((element, nearest)) = pending.pop() {
            let owner = if element.attr("id").is_some() {
                element
            } else {
                nearest
            };
            if let Some(value) = element.attrs.get(&(XSI.to_string(), "type".to_string())) {
                // A condition already refused for this reason is not listed twice.
                let reported = owner
                    .attr("id")
                    .is_some_and(|id| self.fatal_elements.contains(id));
                if !known.contains(value) && !reported {
                    self.report(
                        Severity::Fatal,
                        "CAMUNDA_UNSUPPORTED_XSI_TYPE",
                        owner,
                        format!("{} has the unsupported type {value}", element.local),
                    );
                }
            }
            pending.extend(element.children.iter().map(|child| (child, owner)));
        }
    }

    // ----- diagram -----

    fn rewrite_diagram(&mut self, diagram: &mut Element) {
        self.drop_diagram_attributes(diagram, &["id"]);
        let before = diagram.children.len();
        diagram
            .children
            .retain(|c| c.ns == BPMNDI && c.local == "BPMNPlane");
        self.diagram_detail_dropped |= diagram.children.len() != before;
        for plane in &mut diagram.children {
            self.drop_diagram_attributes(plane, &["id", "bpmnElement"]);
            let before = plane.children.len();
            plane
                .children
                .retain(|c| c.ns == BPMNDI && matches!(c.local.as_str(), "BPMNShape" | "BPMNEdge"));
            self.diagram_detail_dropped |= plane.children.len() != before;
            for shape in &mut plane.children {
                if shape.local == "BPMNShape" {
                    let expanded = shape
                        .attrs
                        .get(&(String::new(), "isExpanded".into()))
                        .cloned();
                    let keep_expanded = matches!(expanded.as_deref(), Some("false" | "0"));
                    let kept = if keep_expanded {
                        vec!["id", "bpmnElement", "isExpanded"]
                    } else {
                        vec!["id", "bpmnElement"]
                    };
                    self.drop_diagram_attributes(shape, &kept);
                    let before = shape.children.len();
                    shape.children.retain(|c| c.ns == DC && c.local == "Bounds");
                    self.diagram_detail_dropped |= shape.children.len() != before;
                } else {
                    self.drop_diagram_attributes(shape, &["id", "bpmnElement"]);
                    let before = shape.children.len();
                    shape
                        .children
                        .retain(|c| c.ns == DI && c.local == "waypoint");
                    self.diagram_detail_dropped |= shape.children.len() != before;
                }
            }
        }
    }

    fn drop_diagram_attributes(&mut self, element: &mut Element, keep: &[&str]) {
        let before = element.attrs.len();
        element
            .attrs
            .retain(|(ns, name), _| ns.is_empty() && keep.contains(&name.as_str()));
        self.diagram_detail_dropped |= element.attrs.len() != before;
    }

    // ----- per-element verification -----

    /// Runs the strict reader over every flow node on its own, so each element
    /// the strict reader refuses is reported, not only the first. Returns
    /// whether any failed.
    fn check_nodes(&mut self, container: &Element, namespace: &str) -> bool {
        let mut failed = false;
        for child in &container.children {
            let skipped = child.is(BPMN, "sequenceFlow")
                || matches!(
                    child.local.as_str(),
                    "laneSet"
                        | "dataObject"
                        | "dataObjectReference"
                        | "dataStoreReference"
                        | "association"
                        | "textAnnotation"
                        | "extensionElements"
                )
                || (container.is(BPMN, "subProcess")
                    && matches!(
                        child.local.as_str(),
                        "ioSpecification"
                            | "dataInputAssociation"
                            | "dataOutputAssociation"
                            | "multiInstanceLoopCharacteristics"
                            | "standardLoopCharacteristics"
                    ));
            if skipped || child.ns != BPMN {
                continue;
            }
            if child.is(BPMN, "subProcess") && self.check_nodes(child, namespace) {
                failed = true;
                continue;
            }
            if child
                .attr("id")
                .is_some_and(|id| self.fatal_elements.contains(id))
            {
                failed = true;
                continue;
            }
            if let Err(error) = node_from_xml(child, namespace) {
                failed = true;
                let known = error.downcast_ref::<XmlElementError>();
                self.push(
                    Severity::Fatal,
                    "CAMUNDA_ELEMENT_UNSUPPORTED",
                    known
                        .and_then(|known| known.element_id.clone())
                        .or_else(|| child.attr("id").map(str::to_string)),
                    Some(known.map_or(child.offset, |known| known.offset)),
                    format!("{} cannot be imported: {error}", child.local),
                );
            }
        }
        failed
    }
}

#[derive(Default)]
struct Parameters {
    input: BTreeMap<String, String>,
    output: BTreeMap<String, String>,
    connector: Option<String>,
}

fn diagnostic(
    severity: Severity,
    code: &str,
    element_id: Option<String>,
    offset: Option<usize>,
    message: String,
) -> ProcessDiagnostic {
    ProcessDiagnostic {
        code: code.into(),
        message: clip_to(&message, MAX_MESSAGE_CHARS),
        element_id,
        offset,
        fatal: severity == Severity::Fatal,
        boundary_id: None,
        flow_id: None,
        node_id: None,
        reason: None,
        severity: Some(severity),
    }
}

fn patch_repeat(
    nodes: &mut [ProcessNode],
    variables: &mut BTreeMap<String, serde_json::Value>,
    patch: &RepeatPatch,
) -> bool {
    if let Some(node) = nodes.iter_mut().find(|node| node.id == patch.node_id) {
        node.repeat = Some(patch.spec.clone());
        if let ProcessRepeatSpec::MultiInstance {
            output_collection_variable,
            ..
        } = &patch.spec
        {
            variables
                .entry(output_collection_variable.clone())
                .or_insert_with(|| serde_json::json!([]));
        }
        return true;
    }
    for node in nodes {
        if let ProcessNodeKind::SubProcess { body, .. } = &mut node.kind {
            if patch_repeat(&mut body.nodes, &mut body.variables, patch) {
                return true;
            }
        }
    }
    false
}

fn patch_process_repeat(process: &mut ProcessExecutableProcess, patch: &RepeatPatch) -> bool {
    patch_repeat(&mut process.nodes, &mut process.variables, patch)
}

fn element(
    ns: &str,
    local: &str,
    attrs: &[(&str, &str)],
    children: Vec<Element>,
    text: &str,
    offset: usize,
) -> Element {
    Element {
        ns: ns.into(),
        local: local.into(),
        attrs: attrs
            .iter()
            .map(|(name, value)| ((String::new(), (*name).to_string()), (*value).to_string()))
            .collect(),
        qnames: Default::default(),
        children,
        text: text.into(),
        offset,
    }
}

fn json_element(local: &str, mapping: &BTreeMap<String, String>, offset: usize) -> Element {
    let text = serde_json::to_string(mapping).expect("a string map serializes");
    element(TF, local, &[], Vec::new(), &text, offset)
}

fn take_camunda(element: &mut Element, name: &str) -> Option<String> {
    element
        .attrs
        .remove(&(CAMUNDA_NAMESPACE.to_string(), name.to_string()))
}

fn has_wrapper(text: &str) -> bool {
    text.contains("${") || text.contains("#{")
}

fn is_plain_name(text: &str) -> bool {
    !text.is_empty()
        && text.as_bytes()[0].is_ascii_alphabetic()
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `<element id>_results` as a TentaFlow variable name.
fn results_variable(id: &str) -> String {
    let mut name: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    name.truncate(100);
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        name.insert_str(0, "v_");
    }
    format!("{name}_results")
}

fn bounded_origin(origin: &str) -> String {
    let mut clean: String = origin.chars().filter(|c| !c.is_control()).collect();
    if clean.len() > crate::processes::model::MAX_UNBOUND_ORIGIN_BYTES {
        let mut end = crate::processes::model::MAX_UNBOUND_ORIGIN_BYTES;
        while !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean.truncate(end);
    }
    clean
}

fn clip_to(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(limit).collect();
    clipped.push('…');
    clipped
}

fn clip(text: &str) -> String {
    const LIMIT: usize = 80;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(LIMIT).collect();
    clipped.push('…');
    clipped
}

fn describe_listener(listener: &Element) -> String {
    let event = listener.attr("event").unwrap_or("?");
    let target = ["class", "delegateExpression", "expression"]
        .iter()
        .find_map(|name| {
            listener
                .attr(name)
                .map(|value| format!("{name} {}", clip(value)))
        })
        .unwrap_or_else(|| "script".into());
    format!("({event}: {target})")
}

/// A CEL string literal for text that Camunda treats as a plain string.
fn cel_string(text: &str) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for c in text.chars() {
        match c {
            '\\' => literal.push_str("\\\\"),
            '"' => literal.push_str("\\\""),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            c if c.is_control() => literal.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => literal.push(c),
        }
    }
    literal.push('"');
    literal
}

/// Number of `documentation` children at the front, where the strict reader
/// expects them ahead of the extension.
fn leading_documentation(children: &[Element]) -> usize {
    children
        .iter()
        .take_while(|child| child.is(BPMN, "documentation"))
        .count()
}

fn collect_ids(element: &Element, ids: &mut Vec<String>) {
    if let Some(id) = element.attr("id") {
        ids.push(id.to_string());
    }
    for child in &element.children {
        collect_ids(child, ids);
    }
}

/// Every variable name a node set declares or writes, at any depth.
fn collect_names(
    nodes: &[ProcessNode],
    variables: &BTreeMap<String, serde_json::Value>,
    taken: &mut BTreeSet<String>,
) {
    taken.extend(variables.keys().cloned());
    for node in nodes {
        written_names(node, taken);
        if let ProcessNodeKind::SubProcess { body, .. } = &node.kind {
            collect_names(&body.nodes, &body.variables, taken);
        }
    }
}

/// Names a node writes into its scope, nested subprocess bodies excluded.
fn written_names(node: &ProcessNode, names: &mut BTreeSet<String>) {
    let mapping = match &node.kind {
        ProcessNodeKind::UserTask { output_mapping, .. }
        | ProcessNodeKind::ServiceTask { output_mapping, .. }
        | ProcessNodeKind::ScriptTask { output_mapping, .. }
        | ProcessNodeKind::SubProcess { output_mapping, .. }
        | ProcessNodeKind::MessageStart { output_mapping, .. }
        | ProcessNodeKind::MessageCatch { output_mapping, .. }
        | ProcessNodeKind::ReceiveTask { output_mapping, .. }
        | ProcessNodeKind::SignalCatch { output_mapping, .. }
        | ProcessNodeKind::BoundaryMessage { output_mapping, .. }
        | ProcessNodeKind::BoundaryError { output_mapping, .. }
        | ProcessNodeKind::BoundaryEscalation { output_mapping, .. } => Some(output_mapping),
        _ => None,
    };
    if let Some(mapping) = mapping {
        names.extend(mapping.keys().cloned());
    }
    if let Some(
        ProcessRepeatSpec::MultiInstance {
            output_collection_variable,
            ..
        }
        | ProcessRepeatSpec::StructuredLoop {
            output_collection_variable,
            ..
        },
    ) = &node.repeat
    {
        names.insert(output_collection_variable.clone());
    }
}

/// `base`, or `base_2`, `base_3`, ... — the first not in `taken`.
fn free_name(base: &str, taken: &BTreeSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}_{n}"))
        .find(|candidate| !taken.contains(candidate))
        .expect("an unbounded counter finds a free name")
}

fn outputs_reference(name: &str) -> String {
    let plain = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        format!("outputs.{name}")
    } else {
        format!("outputs[{}]", cel_string(name))
    }
}

fn vars_reference(name: &str) -> String {
    outputs_reference(name).replacen("outputs", "vars", 1)
}

/// What `propagate_subprocess_outputs` could not settle by itself.
#[derive(Default)]
struct Propagation {
    /// Subprocesses holding steps whose real implementation is unknown.
    uncertain: Vec<String>,
    /// Subprocesses that would need more mapping keys than a model allows.
    too_many: Vec<String>,
}

/// Camunda lets a variable set inside an embedded subprocess flow up the scope
/// chain; TentaFlow keeps the body's variables local. For every subprocess this
/// importer created, each name written inside the body is declared on both
/// sides, handed in (`input_mapping`, so the body reads the parent's value
/// until it overwrites it; a local null would shadow it) and copied out.
fn propagate_subprocess_outputs(
    nodes: &mut [ProcessNode],
    variables: &mut BTreeMap<String, serde_json::Value>,
    synthesized: &HashSet<String>,
    result: &mut Propagation,
) {
    for node in nodes.iter_mut() {
        let id = node.id.clone();
        let ProcessNodeKind::SubProcess {
            body,
            input_mapping,
            output_mapping,
        } = &mut node.kind
        else {
            continue;
        };
        propagate_subprocess_outputs(&mut body.nodes, &mut body.variables, synthesized, result);
        if !synthesized.contains(&id) {
            continue;
        }
        let mut written = BTreeSet::new();
        for inner in &body.nodes {
            written_names(inner, &mut written);
        }
        for name in written {
            body.variables
                .entry(name.clone())
                .or_insert(serde_json::Value::Null);
            variables
                .entry(name.clone())
                .or_insert(serde_json::Value::Null);
            input_mapping
                .entry(name.clone())
                .or_insert_with(|| vars_reference(&name));
            output_mapping
                .entry(name.clone())
                .or_insert_with(|| outputs_reference(&name));
        }
        if input_mapping.len() > MAX_VARIABLE_KEYS
            || output_mapping.len() > MAX_VARIABLE_KEYS
            || body.variables.len() > MAX_VARIABLE_KEYS
        {
            result.too_many.push(id.clone());
        }
        if holds_unknown_steps(&body.nodes) {
            result.uncertain.push(id);
        }
    }
}

fn holds_unknown_steps(nodes: &[ProcessNode]) -> bool {
    nodes.iter().any(|node| match &node.kind {
        ProcessNodeKind::ServiceTask {
            implementation: ServiceImplementation::Unbound { .. },
            ..
        }
        | ProcessNodeKind::CallActivity(_) => true,
        ProcessNodeKind::SubProcess { body, .. } => holds_unknown_steps(&body.nodes),
        _ => false,
    })
}
