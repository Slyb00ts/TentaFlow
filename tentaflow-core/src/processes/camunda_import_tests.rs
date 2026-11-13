// ===== File: processes/camunda_import_tests.rs — tolerant Camunda 7 import: one fixture per mapping row, hostile input and round trips =====

use std::collections::BTreeMap;

use tentaflow_protocol::processes::{
    ProcessDiagnostic, ProcessDiagnosticSeverity as Severity, ProcessModel,
    ProcessMultiInstanceInput, ProcessMultiInstanceMode, ProcessNode, ProcessNodeKind,
    ProcessRepeatSpec, ProcessRetryBackoff, ProcessRetryPolicy, ServiceImplementation,
};

use super::bpmn::{
    declares_camunda_namespace, export_xml, import_camunda_xml, import_camunda_xml_resolving,
    import_document, import_xml, CamundaImport,
};
use super::model::{all_nodes, starter_model, validate_draft, validate_model};
use super::runtime::test_support::{start_model, Fixture};

const HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:bpmndi="http://www.omg.org/spec/BPMN/20100524/DI" xmlns:dc="http://www.omg.org/spec/DD/20100524/DC" xmlns:di="http://www.omg.org/spec/DD/20100524/DI" xmlns:camunda="http://camunda.org/schema/1.0/bpmn" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" id="Definitions_1" targetNamespace="http://bpmn.io/schema/bpmn" exporter="Camunda Modeler" exporterVersion="5.0.0">"#;

fn process(body: &str) -> String {
    format!(
        "{HEAD}<bpmn:process id=\"Process_1\" name=\"Order\" isExecutable=\"true\" \
         camunda:historyTimeToLive=\"180\">{body}</bpmn:process></bpmn:definitions>"
    )
}

/// start -> `Task_1` -> end, written the way Camunda Modeler writes it.
fn linear(task: &str) -> String {
    process(&format!(
        r#"<bpmn:startEvent id="Start_1"><bpmn:outgoing>Flow_1</bpmn:outgoing></bpmn:startEvent>
        {task}
        <bpmn:endEvent id="End_1"><bpmn:incoming>Flow_2</bpmn:incoming></bpmn:endEvent>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Task_1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Task_1" targetRef="End_1"/>"#
    ))
}

/// start -> split -> (`Task_A` | `Task_B`) -> join -> end with the given
/// condition elements on the two split flows.
fn gateway(condition_a: &str, condition_b: &str) -> String {
    process(&format!(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:exclusiveGateway id="Split_1"/>
        <bpmn:userTask id="Task_A"/>
        <bpmn:userTask id="Task_B"/>
        <bpmn:exclusiveGateway id="Join_1"/>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_0" sourceRef="Start_1" targetRef="Split_1"/>
        <bpmn:sequenceFlow id="Flow_A" name="yes" sourceRef="Split_1" targetRef="Task_A">{condition_a}</bpmn:sequenceFlow>
        <bpmn:sequenceFlow id="Flow_B" sourceRef="Split_1" targetRef="Task_B">{condition_b}</bpmn:sequenceFlow>
        <bpmn:sequenceFlow id="Flow_A2" sourceRef="Task_A" targetRef="Join_1"/>
        <bpmn:sequenceFlow id="Flow_B2" sourceRef="Task_B" targetRef="Join_1"/>
        <bpmn:sequenceFlow id="Flow_E" sourceRef="Join_1" targetRef="End_1"/>"#
    ))
}

fn formal(language: &str, text: &str) -> String {
    let language = if language.is_empty() {
        String::new()
    } else {
        format!(" language=\"{language}\"")
    };
    format!(
        "<bpmn:conditionExpression xsi:type=\"bpmn:tFormalExpression\"{language}>{text}</bpmn:conditionExpression>"
    )
}

fn node<'a>(model: &'a ProcessModel, id: &str) -> &'a ProcessNode {
    all_nodes(model)
        .into_iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("node {id} missing"))
}

fn diagnostics<'a>(imported: &'a CamundaImport, code: &str) -> Vec<&'a ProcessDiagnostic> {
    imported
        .diagnostics
        .iter()
        .filter(|d| d.code == code)
        .collect()
}

fn only<'a>(imported: &'a CamundaImport, code: &str) -> &'a ProcessDiagnostic {
    let found = diagnostics(imported, code);
    assert_eq!(found.len(), 1, "{code} in {:#?}", imported.diagnostics);
    found[0]
}

fn assert_clean_import(imported: &CamundaImport) -> &ProcessModel {
    assert!(
        imported.diagnostics.iter().all(|d| !d.fatal),
        "unexpected fatal: {:#?}",
        imported.diagnostics
    );
    imported.model.as_ref().expect("model imported")
}

fn retry_of(model: &ProcessModel, id: &str) -> Option<ProcessRetryPolicy> {
    match &node(model, id).kind {
        ProcessNodeKind::ServiceTask { retry, .. } => retry.clone(),
        other => panic!("{id} is not a service task: {other:?}"),
    }
}

fn service_implementation(model: &ProcessModel, id: &str) -> ServiceImplementation {
    match &node(model, id).kind {
        ProcessNodeKind::ServiceTask { implementation, .. } => implementation.clone(),
        other => panic!("{id} is not a service task: {other:?}"),
    }
}

fn condition_of<'a>(model: &'a ProcessModel, flow: &str) -> Option<&'a str> {
    model
        .sequence_flows
        .iter()
        .find(|candidate| candidate.id == flow)
        .unwrap_or_else(|| panic!("flow {flow} missing"))
        .condition
        .as_deref()
}

// ----- severity representation -----

#[test]
fn every_camunda_diagnostic_carries_a_severity_that_agrees_with_fatal() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:scriptTask id="Task_1" scriptFormat="javascript" camunda:asyncBefore="true"><bpmn:script>x = 1</bpmn:script></bpmn:scriptTask>"#,
    ));
    assert!(!imported.diagnostics.is_empty());
    for diagnostic in &imported.diagnostics {
        let severity = diagnostic
            .severity
            .expect("tolerant diagnostics are graded");
        assert_eq!(
            diagnostic.fatal,
            severity == Severity::Fatal,
            "{diagnostic:?}"
        );
        assert!(
            diagnostic.code.starts_with("CAMUNDA_"),
            "{}",
            diagnostic.code
        );
    }
}

#[test]
fn strict_diagnostics_stay_ungraded() {
    let (_, diagnostics) = import_xml("<not-bpmn/>");
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].fatal);
    assert_eq!(diagnostics[0].severity, None);
    assert_eq!(diagnostics[0].element_name, None);
}

// ----- namespace detection and dispatch -----

#[test]
fn the_camunda_namespace_is_detected_on_the_root_only() {
    assert!(declares_camunda_namespace(&process("")));
    assert!(declares_camunda_namespace(
        r#"<?xml version="1.0"?><definitions xmlns="http://camunda.org/schema/1.0/bpmn"/>"#
    ));
    assert!(!declares_camunda_namespace(
        r#"<d xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL"><x xmlns:camunda="http://camunda.org/schema/1.0/bpmn"/></d>"#
    ));
    assert!(!declares_camunda_namespace("not xml"));
    assert!(!declares_camunda_namespace(""));
}

#[test]
fn import_document_leaves_a_non_camunda_document_to_the_strict_reader() {
    let xml = export_xml(&starter_model()).unwrap();
    let (strict_model, strict_diagnostics) = import_xml(&xml);
    let routed = import_document(&xml, &|_| None);
    assert_eq!(routed.model, strict_model);
    assert_eq!(routed.diagnostics, strict_diagnostics);
    assert!(routed.translations.is_empty());
    let bad = import_document("<not-bpmn/>", &|_| None);
    assert_eq!(bad.diagnostics, import_xml("<not-bpmn/>").1);
}

#[test]
fn a_document_declaring_the_namespace_without_camunda_constructs_imports_like_the_strict_reader() {
    let xml = export_xml(&starter_model()).unwrap();
    let declared = xml.replacen(
        "<bpmn:definitions",
        "<bpmn:definitions xmlns:camunda=\"http://camunda.org/schema/1.0/bpmn\"",
        1,
    );
    assert!(declares_camunda_namespace(&declared));
    let (strict_model, strict_diagnostics) = import_xml(&xml);
    assert!(strict_diagnostics.is_empty());
    let tolerant = import_camunda_xml(&declared);
    assert_eq!(tolerant.model, strict_model);
    assert!(
        tolerant.diagnostics.is_empty(),
        "{:#?}",
        tolerant.diagnostics
    );
    assert!(tolerant.translations.is_empty());
}

// ----- mapping table: user tasks -----

#[test]
fn assignee_that_resolves_to_a_user_is_imported() {
    let xml = linear(r#"<bpmn:userTask id="Task_1" name="Approve" camunda:assignee="jkowalski"/>"#);
    let imported = import_camunda_xml_resolving(&xml, &|login| {
        (login == "jkowalski").then(|| "user-42".to_string())
    });
    let model = assert_clean_import(&imported);
    assert!(diagnostics(&imported, "CAMUNDA_ASSIGNEE_UNRESOLVED").is_empty());
    assert!(matches!(
        &node(model, "Task_1").kind,
        ProcessNodeKind::UserTask { assignee_user_id: Some(id), .. } if id == "user-42"
    ));
}

#[test]
fn assignee_that_does_not_resolve_is_a_warning_naming_the_login() {
    let xml = linear(r#"<bpmn:userTask id="Task_1" camunda:assignee="ghost"/>"#);
    let imported = import_camunda_xml(&xml);
    let model = assert_clean_import(&imported);
    let warning = only(&imported, "CAMUNDA_ASSIGNEE_UNRESOLVED");
    assert_eq!(warning.severity, Some(Severity::Warning));
    assert_eq!(warning.element_id.as_deref(), Some("Task_1"));
    assert!(warning.message.contains("ghost"));
    assert!(matches!(
        &node(model, "Task_1").kind,
        ProcessNodeKind::UserTask {
            assignee_user_id: None,
            ..
        }
    ));
}

#[test]
fn an_expression_assignee_is_never_looked_up_and_gets_its_own_warning() {
    let xml = linear(r#"<bpmn:userTask id="Task_1" camunda:assignee="${approver}"/>"#);
    let imported = import_camunda_xml_resolving(&xml, &|_| panic!("an expression is not a login"));
    assert_clean_import(&imported);
    assert!(diagnostics(&imported, "CAMUNDA_ASSIGNEE_UNRESOLVED").is_empty());
    let warning = only(&imported, "CAMUNDA_ASSIGNEE_EXPRESSION");
    assert_eq!(warning.severity, Some(Severity::Warning));
    assert_eq!(warning.element_id.as_deref(), Some("Task_1"));
    assert!(
        warning.message.contains("process initiator"),
        "{}",
        warning.message
    );
}

#[test]
fn an_assignee_expression_reading_the_element_variable_is_a_read_and_says_the_per_item_assignment_is_lost(
) {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:userTask id="Task_1" camunda:assignee="${approver}"><bpmn:multiInstanceLoopCharacteristics camunda:collection="approvers" camunda:elementVariable="approver"/></bpmn:userTask>"#,
    ));
    let warning = only(&imported, "CAMUNDA_ASSIGNEE_EXPRESSION");
    assert!(warning.message.contains("per-item"), "{}", warning.message);
    // The same expression is a read of the element variable, so the two agree.
    assert!(imported.model.is_none());
    assert_eq!(
        only(&imported, "CAMUNDA_MULTI_INSTANCE_ELEMENT_VARIABLE").severity,
        Some(Severity::Fatal)
    );
}

#[test]
fn an_assignee_reading_the_variable_of_an_enclosing_multi_instance_subprocess_is_found() {
    let imported = import_camunda_xml(&process(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:subProcess id="Sub_1">
          <bpmn:multiInstanceLoopCharacteristics camunda:collection="approvers" camunda:elementVariable="approver"/>
          <bpmn:startEvent id="SubStart_1"/>
          <bpmn:userTask id="Inner_1" camunda:assignee="${approver}"/>
          <bpmn:endEvent id="SubEnd_1"/>
          <bpmn:sequenceFlow id="SF_1" sourceRef="SubStart_1" targetRef="Inner_1"/>
          <bpmn:sequenceFlow id="SF_2" sourceRef="Inner_1" targetRef="SubEnd_1"/>
        </bpmn:subProcess>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Sub_1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Sub_1" targetRef="End_1"/>"#,
    ));
    assert!(only(&imported, "CAMUNDA_ASSIGNEE_EXPRESSION")
        .message
        .contains("per-item"));
    let fatal = only(&imported, "CAMUNDA_MULTI_INSTANCE_ELEMENT_VARIABLE");
    assert_eq!(fatal.severity, Some(Severity::Fatal));
    assert_eq!(fatal.element_id.as_deref(), Some("Sub_1"));
}

#[test]
fn an_unresolved_login_says_the_task_goes_to_the_process_initiator() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:userTask id="Task_1" camunda:assignee="ghost"/>"#,
    ));
    let warning = only(&imported, "CAMUNDA_ASSIGNEE_UNRESOLVED");
    assert!(
        warning.message.contains("process initiator"),
        "{}",
        warning.message
    );
    assert!(!warning.message.contains("unassigned"));
}

#[test]
fn candidates_forms_and_form_data_are_warnings_and_skipped() {
    let xml = linear(
        r#"<bpmn:userTask id="Task_1" camunda:candidateUsers="a,b" camunda:candidateGroups="sales" camunda:formKey="app:form.html" camunda:dueDate="${due}">
          <bpmn:extensionElements><camunda:formData><camunda:formField id="f" label="F" type="string"/></camunda:formData></bpmn:extensionElements>
        </bpmn:userTask>"#,
    );
    let imported = import_camunda_xml(&xml);
    assert_clean_import(&imported);
    let candidates = diagnostics(&imported, "CAMUNDA_CANDIDATES_SKIPPED");
    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .all(|d| d.severity == Some(Severity::Warning)));
    // The form key (attribute) and the form data (element) are reported separately.
    let forms = diagnostics(&imported, "CAMUNDA_FORM_SKIPPED");
    assert_eq!(forms.len(), 2);
    assert!(forms.iter().all(|d| d.severity == Some(Severity::Warning)));
    only(&imported, "CAMUNDA_USER_TASK_ATTRIBUTE_SKIPPED");
}

// ----- mapping table: input/output parameters -----

#[test]
fn text_and_expression_parameters_become_mappings_with_translations() {
    let xml = linear(
        r#"<bpmn:serviceTask id="Task_1" camunda:delegateExpression="${mailer}">
          <bpmn:extensionElements><camunda:inputOutput>
            <camunda:inputParameter name="greeting">Hello "world"</camunda:inputParameter>
            <camunda:inputParameter name="amount">${order.total}</camunda:inputParameter>
            <camunda:outputParameter name="mailId">${messageId}</camunda:outputParameter>
          </camunda:inputOutput></bpmn:extensionElements>
        </bpmn:serviceTask>"#,
    );
    let imported = import_camunda_xml(&xml);
    let model = assert_clean_import(&imported);
    let ProcessNodeKind::ServiceTask {
        input_mapping,
        output_mapping,
        ..
    } = &node(model, "Task_1").kind
    else {
        panic!("service task expected");
    };
    assert_eq!(input_mapping["greeting"], r#""Hello \"world\"""#);
    assert!(
        input_mapping["amount"].contains("vars.order.total"),
        "{input_mapping:?}"
    );
    assert!(
        output_mapping["mailId"].contains("vars.messageId"),
        "{output_mapping:?}"
    );

    let by_kind = |kind: &str| -> Vec<_> {
        imported
            .translations
            .iter()
            .filter(|t| t.kind == kind)
            .collect()
    };
    assert_eq!(by_kind("input_parameter").len(), 2);
    assert_eq!(by_kind("output_parameter").len(), 1);
    let amount = by_kind("input_parameter")
        .into_iter()
        .find(|t| t.original == "${order.total}")
        .expect("expression translation listed");
    assert_eq!(amount.translated, input_mapping["amount"]);
    assert_eq!(amount.element_id, "Task_1");
    // An output parameter is evaluated in another scope than Camunda's.
    only(&imported, "CAMUNDA_OUTPUT_PARAMETER_SCOPE");
}

#[test]
fn user_task_output_parameters_become_its_output_mapping() {
    let xml = linear(
        r#"<bpmn:userTask id="Task_1"><bpmn:extensionElements><camunda:inputOutput>
            <camunda:inputParameter name="ignored">x</camunda:inputParameter>
            <camunda:outputParameter name="decision">${approved}</camunda:outputParameter>
        </camunda:inputOutput></bpmn:extensionElements></bpmn:userTask>"#,
    );
    let imported = import_camunda_xml(&xml);
    let model = assert_clean_import(&imported);
    let ProcessNodeKind::UserTask { output_mapping, .. } = &node(model, "Task_1").kind else {
        panic!("user task expected");
    };
    assert!(output_mapping["decision"].contains("vars.approved"));
    // A user task has no input mapping.
    only(&imported, "CAMUNDA_PARAMETERS_SKIPPED");
}

#[test]
fn list_map_script_and_malformed_parameters_are_warnings_and_skipped() {
    let xml = linear(
        r#"<bpmn:serviceTask id="Task_1" camunda:expression="${svc.run()}">
          <bpmn:extensionElements><camunda:inputOutput>
            <camunda:inputParameter name="items"><camunda:list><camunda:value>1</camunda:value></camunda:list></camunda:inputParameter>
            <camunda:inputParameter name="dict"><camunda:map><camunda:entry key="k">v</camunda:entry></camunda:map></camunda:inputParameter>
            <camunda:inputParameter name="calc"><camunda:script scriptFormat="groovy">1+1</camunda:script></camunda:inputParameter>
            <camunda:inputParameter name="mixed">total: ${order.total}</camunda:inputParameter>
            <camunda:inputParameter name="empty"></camunda:inputParameter>
            <camunda:inputParameter name="bad name">x</camunda:inputParameter>
            <camunda:inputParameter name="dup">1</camunda:inputParameter>
            <camunda:inputParameter name="dup">2</camunda:inputParameter>
            <camunda:inputParameter name="kept">ok</camunda:inputParameter>
          </camunda:inputOutput></bpmn:extensionElements>
        </bpmn:serviceTask>"#,
    );
    let imported = import_camunda_xml(&xml);
    let model = assert_clean_import(&imported);
    assert_eq!(
        diagnostics(&imported, "CAMUNDA_PARAMETER_STRUCTURE_SKIPPED").len(),
        2
    );
    only(&imported, "CAMUNDA_PARAMETER_SCRIPT_SKIPPED");
    let refused = only(&imported, "CAMUNDA_EXPRESSION_REFUSED");
    assert!(
        refused.message.contains("EXPR_COMPOSITE_TEMPLATE"),
        "{}",
        refused.message
    );
    assert_eq!(refused.severity, Some(Severity::Warning));
    assert_eq!(diagnostics(&imported, "CAMUNDA_PARAMETER_SKIPPED").len(), 3);
    let ProcessNodeKind::ServiceTask { input_mapping, .. } = &node(model, "Task_1").kind else {
        panic!("service task expected");
    };
    assert_eq!(
        input_mapping.keys().map(String::as_str).collect::<Vec<_>>(),
        ["dup", "kept"]
    );
    assert_eq!(input_mapping["dup"], "\"1\"");
}

// ----- mapping table: service tasks -----

fn origin_of(xml: &str) -> (String, CamundaImport) {
    let imported = import_camunda_xml(xml);
    let model = assert_clean_import(&imported);
    let ServiceImplementation::Unbound { origin } = service_implementation(model, "Task_1") else {
        panic!("an unbound step was expected");
    };
    (origin, imported)
}

#[test]
fn each_service_task_form_becomes_an_unbound_step_recording_its_origin() {
    let cases = [
        (
            r#"camunda:delegateExpression="${orderBean}""#,
            "delegateExpression:${orderBean}",
        ),
        (r#"camunda:class="com.acme.Order""#, "class:com.acme.Order"),
        (
            r#"camunda:expression="${orderService.run(execution)}""#,
            "expression:${orderService.run(execution)}",
        ),
        (
            r#"camunda:type="external" camunda:topic="charge-card""#,
            "external:charge-card",
        ),
        (r#"camunda:type="mail""#, "type:mail"),
    ];
    for (attributes, expected) in cases {
        let (origin, imported) = origin_of(&linear(&format!(
            r#"<bpmn:serviceTask id="Task_1" {attributes} camunda:asyncBefore="true"/>"#
        )));
        assert_eq!(origin, expected);
        let warning = only(&imported, "CAMUNDA_SERVICE_UNBOUND");
        assert_eq!(warning.severity, Some(Severity::Warning));
        assert_eq!(warning.element_id.as_deref(), Some("Task_1"));
        assert!(warning.message.contains(expected), "{}", warning.message);
    }
}

#[test]
fn a_connector_and_a_service_task_without_implementation_are_unbound_too() {
    let (origin, _) = origin_of(&linear(
        r#"<bpmn:serviceTask id="Task_1"><bpmn:extensionElements><camunda:connector>
            <camunda:connectorId>http-connector</camunda:connectorId>
        </camunda:connector></bpmn:extensionElements></bpmn:serviceTask>"#,
    ));
    assert_eq!(origin, "connector:http-connector");
    let (origin, imported) = origin_of(&linear(r#"<bpmn:serviceTask id="Task_1"/>"#));
    assert_eq!(origin, "unspecified");
    only(&imported, "CAMUNDA_SERVICE_NO_IMPLEMENTATION");
}

#[test]
fn a_result_variable_of_a_service_task_maps_the_step_result() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:serviceTask id="Task_1" camunda:expression="${x.y()}" camunda:resultVariable="answer"/>"#,
    ));
    let model = assert_clean_import(&imported);
    let ProcessNodeKind::ServiceTask { output_mapping, .. } = &node(model, "Task_1").kind else {
        panic!("service task expected");
    };
    assert_eq!(output_mapping["answer"], "outputs");
}

#[test]
fn an_overlong_origin_is_bounded_not_refused() {
    let class = "a".repeat(2000);
    let imported = import_camunda_xml(&linear(&format!(
        r#"<bpmn:serviceTask id="Task_1" camunda:class="{class}"/>"#
    )));
    let model = assert_clean_import(&imported);
    let ServiceImplementation::Unbound { origin } = service_implementation(model, "Task_1") else {
        panic!("unbound expected");
    };
    assert_eq!(origin.len(), super::model::MAX_UNBOUND_ORIGIN_BYTES);
}

// ----- unbound semantics -----

fn unbound_model() -> ProcessModel {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:serviceTask id="Task_1" camunda:delegateExpression="${bean}"/>"#,
    ));
    assert_clean_import(&imported).clone()
}

#[test]
fn a_draft_with_an_unbound_step_saves_but_never_publishes() {
    let model = unbound_model();
    validate_draft(&model).expect("a draft may carry an unbound step");
    let error = validate_model(&model).unwrap_err().to_string();
    assert!(error.contains("UNBOUND_IMPLEMENTATION"), "{error}");
    assert!(error.contains("Task_1"), "{error}");
}

#[test]
fn an_unbound_step_inside_a_subprocess_is_refused_for_publication_too() {
    let imported = import_camunda_xml(&process(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:subProcess id="Sub_1">
          <bpmn:startEvent id="SubStart_1"/>
          <bpmn:serviceTask id="Task_1" camunda:class="x.Y"/>
          <bpmn:endEvent id="SubEnd_1"/>
          <bpmn:sequenceFlow id="SubFlow_1" sourceRef="SubStart_1" targetRef="Task_1"/>
          <bpmn:sequenceFlow id="SubFlow_2" sourceRef="Task_1" targetRef="SubEnd_1"/>
        </bpmn:subProcess>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Sub_1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Sub_1" targetRef="End_1"/>"#,
    ));
    let model = assert_clean_import(&imported);
    let error = format!("{:#}", validate_model(model).unwrap_err());
    assert!(
        error.contains("UNBOUND_IMPLEMENTATION") && error.contains("Task_1"),
        "{error}"
    );
}

#[test]
fn an_unbound_step_survives_export_and_strict_reimport() {
    let model = unbound_model();
    let xml = export_xml(&model).expect("a draft with an unbound step exports");
    assert!(
        xml.contains(r#"implementation="unbound""#) && xml.contains("delegateExpression:${bean}")
    );
    let (reimported, diagnostics) = import_xml(&xml);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(reimported.as_ref(), Some(&model));
}

#[test]
fn strict_import_rejects_an_unbound_step_that_names_an_implementation() {
    let xml = export_xml(&unbound_model()).unwrap();
    let with_flow = xml.replace(
        r#"implementation="unbound""#,
        r#"implementation="unbound" flowId="f""#,
    );
    let (model, diagnostics) = import_xml(&with_flow);
    assert!(model.is_none() && diagnostics[0].fatal);
}

// ----- mapping table: script tasks -----

#[test]
fn foreign_script_languages_are_fatal_for_the_element() {
    for format in ["javascript", "groovy", "python", "ruby", ""] {
        let attribute = if format.is_empty() {
            String::new()
        } else {
            format!(r#"scriptFormat="{format}""#)
        };
        let imported = import_camunda_xml(&linear(&format!(
            r#"<bpmn:scriptTask id="Task_1" {attribute}><bpmn:script>execution.setVariable("a", 1)</bpmn:script></bpmn:scriptTask>"#
        )));
        assert!(imported.model.is_none(), "{format}");
        let fatal = only(&imported, "CAMUNDA_SCRIPT_UNSUPPORTED");
        assert_eq!(fatal.severity, Some(Severity::Fatal));
        assert_eq!(fatal.element_id.as_deref(), Some("Task_1"));
    }
}

#[test]
fn an_external_script_resource_is_fatal() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:scriptTask id="Task_1" scriptFormat="juel" camunda:resource="deployment://a.juel"/>"#,
    ));
    assert!(imported.model.is_none());
    only(&imported, "CAMUNDA_SCRIPT_UNSUPPORTED");
}

#[test]
fn a_juel_script_that_is_a_pure_expression_becomes_a_script_task() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:scriptTask id="Task_1" scriptFormat="juel" camunda:resultVariable="total"><bpmn:script>${price * 2}</bpmn:script></bpmn:scriptTask>"#,
    ));
    let model = assert_clean_import(&imported);
    let ProcessNodeKind::ScriptTask {
        script,
        output_mapping,
    } = &node(model, "Task_1").kind
    else {
        panic!("script task expected");
    };
    let translation = imported
        .translations
        .iter()
        .find(|t| t.kind == "script")
        .expect("script translation listed");
    assert_eq!(&translation.translated, script);
    assert!(script.contains("vars.price"));
    assert_eq!(
        output_mapping,
        &BTreeMap::from([("total".to_string(), "outputs".to_string())])
    );
}

#[test]
fn a_juel_script_the_translator_refuses_is_fatal() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:scriptTask id="Task_1" scriptFormat="juel"><bpmn:script>${execution.setVariable('a', 1)}</bpmn:script></bpmn:scriptTask>"#,
    ));
    assert!(imported.model.is_none());
    let fatal = only(&imported, "CAMUNDA_EXPRESSION_REFUSED");
    assert_eq!(fatal.severity, Some(Severity::Fatal));
    assert!(
        fatal.message.contains("EXPR_UNSUPPORTED_CONSTRUCT"),
        "{}",
        fatal.message
    );
}

#[test]
fn a_tentaflow_cel_script_passes_through_untouched() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:scriptTask id="Task_1" scriptFormat="application/vnd.tentaflow.cel"><bpmn:script>vars.a + 1</bpmn:script></bpmn:scriptTask>"#,
    ));
    let model = assert_clean_import(&imported);
    assert!(matches!(
        &node(model, "Task_1").kind,
        ProcessNodeKind::ScriptTask { script, .. } if script == "vars.a + 1"
    ));
    assert!(imported.translations.is_empty());
}

// ----- mapping table: multi-instance -----

#[test]
fn a_collection_multi_instance_becomes_a_repeat() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:userTask id="Task_1"><bpmn:multiInstanceLoopCharacteristics isSequential="true" camunda:collection="${approvers}" camunda:elementVariable="approver" camunda:asyncBefore="true"/></bpmn:userTask>"#,
    ));
    let model = assert_clean_import(&imported);
    let Some(ProcessRepeatSpec::MultiInstance {
        mode,
        input,
        output_collection_variable,
    }) = &node(model, "Task_1").repeat
    else {
        panic!("multi-instance repeat expected");
    };
    assert_eq!(*mode, ProcessMultiInstanceMode::Sequential);
    let ProcessMultiInstanceInput::CollectionExpression { expression } = input else {
        panic!("collection expected");
    };
    assert!(expression.contains("vars.approvers"));
    assert_eq!(output_collection_variable, "Task_1_results");
    assert!(model.variables.contains_key("Task_1_results"));
    let translation = imported
        .translations
        .iter()
        .find(|t| t.kind == "multi_instance_collection")
        .expect("collection translation listed");
    assert_eq!(translation.original, "${approvers}");
    only(&imported, "CAMUNDA_MULTI_INSTANCE_ELEMENT_VARIABLE");
    only(&imported, "CAMUNDA_MULTI_INSTANCE_OUTPUT_VARIABLE");
    assert_eq!(
        only(&imported, "CAMUNDA_ASYNC_IGNORED").severity,
        Some(Severity::Info)
    );
}

#[test]
fn a_plain_variable_name_collection_and_a_literal_cardinality_are_supported() {
    let by_name = import_camunda_xml(&linear(
        r#"<bpmn:userTask id="Task_1"><bpmn:multiInstanceLoopCharacteristics camunda:collection="items"/></bpmn:userTask>"#,
    ));
    let model = assert_clean_import(&by_name);
    assert!(matches!(
        &node(model, "Task_1").repeat,
        Some(ProcessRepeatSpec::MultiInstance { mode: ProcessMultiInstanceMode::Parallel, input: ProcessMultiInstanceInput::CollectionExpression { expression }, .. })
            if expression.contains("vars.items")
    ));
    let counted = import_camunda_xml(&linear(
        r#"<bpmn:userTask id="Task_1"><bpmn:multiInstanceLoopCharacteristics><bpmn:loopCardinality>3</bpmn:loopCardinality></bpmn:multiInstanceLoopCharacteristics></bpmn:userTask>"#,
    ));
    let model = assert_clean_import(&counted);
    assert!(matches!(
        &node(model, "Task_1").repeat,
        Some(ProcessRepeatSpec::MultiInstance {
            input: ProcessMultiInstanceInput::Cardinality { count: 3 },
            ..
        })
    ));
}

#[test]
fn a_completion_condition_is_fatal_because_it_changes_behavior() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:userTask id="Task_1"><bpmn:multiInstanceLoopCharacteristics camunda:collection="items"><bpmn:completionCondition xsi:type="bpmn:tFormalExpression">${nrOfCompletedInstances &gt;= 2}</bpmn:completionCondition></bpmn:multiInstanceLoopCharacteristics></bpmn:userTask>"#,
    ));
    assert!(imported.model.is_none());
    let fatal = only(&imported, "CAMUNDA_MULTI_INSTANCE_COMPLETION");
    assert_eq!(fatal.severity, Some(Severity::Fatal));
    assert_eq!(fatal.element_id.as_deref(), Some("Task_1"));
}

#[test]
fn a_collection_together_with_a_cardinality_is_fatal() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:userTask id="Task_1"><bpmn:multiInstanceLoopCharacteristics camunda:collection="items"><bpmn:loopCardinality>3</bpmn:loopCardinality></bpmn:multiInstanceLoopCharacteristics></bpmn:userTask>"#,
    ));
    assert!(imported.model.is_none());
    let fatal = only(&imported, "CAMUNDA_MULTI_INSTANCE_INPUT");
    assert_eq!(fatal.element_id.as_deref(), Some("Task_1"));
}

#[test]
fn an_element_variable_read_by_the_activity_is_fatal_and_an_unused_one_a_warning() {
    let read = import_camunda_xml(&linear(
        r#"<bpmn:userTask id="Task_1"><bpmn:extensionElements><camunda:inputOutput><camunda:outputParameter name="seen">${item.name}</camunda:outputParameter></camunda:inputOutput></bpmn:extensionElements><bpmn:multiInstanceLoopCharacteristics camunda:collection="items" camunda:elementVariable="item"/></bpmn:userTask>"#,
    ));
    assert!(read.model.is_none());
    let fatal = only(&read, "CAMUNDA_MULTI_INSTANCE_ELEMENT_VARIABLE");
    assert_eq!(fatal.severity, Some(Severity::Fatal));
    assert_eq!(fatal.element_id.as_deref(), Some("Task_1"));
    // A longer name that merely contains it is not a read of it.
    let other = import_camunda_xml(&linear(
        r#"<bpmn:userTask id="Task_1"><bpmn:extensionElements><camunda:inputOutput><camunda:outputParameter name="seen">${items.size}</camunda:outputParameter></camunda:inputOutput></bpmn:extensionElements><bpmn:multiInstanceLoopCharacteristics camunda:collection="items" camunda:elementVariable="item"/></bpmn:userTask>"#,
    ));
    assert_clean_import(&other);
    assert_eq!(
        only(&other, "CAMUNDA_MULTI_INSTANCE_ELEMENT_VARIABLE").severity,
        Some(Severity::Warning)
    );
}

#[test]
fn colliding_results_variables_get_deterministic_suffixes_and_are_reported() {
    let xml = process(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:userTask id="Task-1"><bpmn:multiInstanceLoopCharacteristics camunda:collection="items"/></bpmn:userTask>
        <bpmn:userTask id="Task.1"><bpmn:multiInstanceLoopCharacteristics camunda:collection="items"/></bpmn:userTask>
        <bpmn:serviceTask id="Task_9" camunda:class="x.Y" camunda:resultVariable="Task_1_results"/>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Task-1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Task-1" targetRef="Task.1"/>
        <bpmn:sequenceFlow id="Flow_3" sourceRef="Task.1" targetRef="Task_9"/>
        <bpmn:sequenceFlow id="Flow_4" sourceRef="Task_9" targetRef="End_1"/>"#,
    );
    let imported = import_camunda_xml(&xml);
    let model = assert_clean_import(&imported);
    let output = |id: &str| match &node(model, id).repeat {
        Some(ProcessRepeatSpec::MultiInstance {
            output_collection_variable,
            ..
        }) => output_collection_variable.clone(),
        other => panic!("{id}: {other:?}"),
    };
    // The step's own result variable owns the plain name; the two repeats that
    // sanitize identically take the next free ones, in document order.
    assert_eq!(output("Task-1"), "Task_1_results_2");
    assert_eq!(output("Task.1"), "Task_1_results_3");
    for name in ["Task_1_results_2", "Task_1_results_3"] {
        assert!(model.variables.contains_key(name));
    }
    let reports = diagnostics(&imported, "CAMUNDA_MULTI_INSTANCE_OUTPUT_VARIABLE");
    assert_eq!(reports.len(), 2);
    assert!(
        reports[0].message.contains("Task_1_results_2")
            && reports[0].message.contains("already in use")
    );
    assert_eq!(reports[1].element_id.as_deref(), Some("Task.1"));
    // And the whole thing is deterministic.
    assert_eq!(import_camunda_xml(&xml).model.as_ref(), Some(model));
}

#[test]
fn a_multi_instance_that_cannot_be_represented_is_fatal() {
    for body in [
        "<bpmn:loopCardinality>${n}</bpmn:loopCardinality>",
        "<bpmn:loopCardinality>17</bpmn:loopCardinality>",
        "",
    ] {
        let imported = import_camunda_xml(&linear(&format!(
            r#"<bpmn:userTask id="Task_1"><bpmn:multiInstanceLoopCharacteristics>{body}</bpmn:multiInstanceLoopCharacteristics></bpmn:userTask>"#
        )));
        assert!(imported.model.is_none(), "{body}");
        assert!(imported.diagnostics.iter().any(|d| d.fatal
            && d.code.starts_with("CAMUNDA_MULTI_INSTANCE_")
            && d.element_id.as_deref() == Some("Task_1")));
    }
}

// ----- mapping table: listeners, async, retries, properties -----

#[test]
fn listeners_are_warnings_and_skipped() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:userTask id="Task_1"><bpmn:extensionElements>
            <camunda:executionListener class="com.acme.Audit" event="start"/>
            <camunda:taskListener delegateExpression="${notifier}" event="create"/>
            <camunda:properties><camunda:property name="k" value="v"/></camunda:properties>
        </bpmn:extensionElements></bpmn:userTask>"#,
    ));
    assert_clean_import(&imported);
    let listeners = diagnostics(&imported, "CAMUNDA_LISTENER_SKIPPED");
    assert_eq!(listeners.len(), 2);
    assert!(listeners
        .iter()
        .all(|d| d.severity == Some(Severity::Warning)));
    assert!(listeners[0].message.contains("com.acme.Audit"));
    assert_eq!(
        only(&imported, "CAMUNDA_PROPERTIES_IGNORED").severity,
        Some(Severity::Info)
    );
}

#[test]
fn async_priority_exclusive_and_history_ttl_are_info_and_retry_cycles_are_warnings() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:serviceTask id="Task_1" camunda:class="x.Y" camunda:asyncBefore="true" camunda:asyncAfter="true" camunda:exclusive="false" camunda:jobPriority="5">
          <bpmn:extensionElements><camunda:failedJobRetryTimeCycle>R3/2024-01-01T00:00:00Z/PT10M</camunda:failedJobRetryTimeCycle></bpmn:extensionElements>
        </bpmn:serviceTask>"#,
    ));
    assert_clean_import(&imported);
    assert_eq!(diagnostics(&imported, "CAMUNDA_ASYNC_IGNORED").len(), 3);
    assert_eq!(
        only(&imported, "CAMUNDA_PRIORITY_IGNORED").severity,
        Some(Severity::Info)
    );
    // `camunda:historyTimeToLive` sits on the process element of the fixture.
    let ttl = only(&imported, "CAMUNDA_HISTORY_TTL_IGNORED");
    assert_eq!(ttl.severity, Some(Severity::Info));
    assert_eq!(ttl.element_id.as_deref(), Some("Process_1"));
    let retry = only(&imported, "CAMUNDA_RETRY_CYCLE_SKIPPED");
    assert_eq!(retry.severity, Some(Severity::Warning));
    assert!(retry.message.contains("R3/2024-01-01T00:00:00Z/PT10M"));
}

// ----- mapping table: conditions -----

#[test]
fn juel_and_default_language_conditions_are_translated_and_listed() {
    let imported = import_camunda_xml(&gateway(
        &formal("", "${approved == true}"),
        &formal("juel", "${!approved}"),
    ));
    let model = assert_clean_import(&imported);
    assert_eq!(
        condition_of(model, "Flow_A").unwrap(),
        "vars.approved == true"
    );
    assert!(condition_of(model, "Flow_B")
        .unwrap()
        .contains("vars.approved"));
    let conditions: Vec<_> = imported
        .translations
        .iter()
        .filter(|t| t.kind == "condition")
        .collect();
    assert_eq!(conditions.len(), 2);
    let first = conditions
        .iter()
        .find(|t| t.element_id == "Flow_A")
        .unwrap();
    assert_eq!(first.original, "${approved == true}");
    assert_eq!(
        Some(first.translated.as_str()),
        condition_of(model, "Flow_A")
    );
}

#[test]
fn feel_conditions_are_translated() {
    let imported = import_camunda_xml(&gateway(
        &formal("feel", "= amount > 100"),
        &formal("FEEL", "= amount &lt;= 100"),
    ));
    let model = assert_clean_import(&imported);
    assert!(condition_of(model, "Flow_A")
        .unwrap()
        .contains("vars.amount"));
    assert_eq!(
        imported
            .translations
            .iter()
            .filter(|t| t.kind == "condition")
            .count(),
        2
    );
}

#[test]
fn a_condition_language_other_than_juel_and_feel_is_fatal_for_that_flow() {
    for language in ["groovy", "javascript", "python", "jruby"] {
        let imported = import_camunda_xml(&gateway(
            &formal(language, "return true"),
            &formal("", "${false}"),
        ));
        assert!(imported.model.is_none(), "{language}");
        let fatal = only(&imported, "CAMUNDA_CONDITION_LANGUAGE");
        assert_eq!(fatal.severity, Some(Severity::Fatal));
        assert_eq!(fatal.element_id.as_deref(), Some("Flow_A"));
        assert!(fatal.message.contains(language));
    }
}

#[test]
fn a_condition_read_from_a_resource_or_typed_foreign_is_fatal() {
    let resource = import_camunda_xml(&gateway(
        r#"<bpmn:conditionExpression xsi:type="bpmn:tFormalExpression" camunda:resource="deployment://c.groovy" language="groovy"/>"#,
        &formal("", "${false}"),
    ));
    assert!(resource.model.is_none());
    only(&resource, "CAMUNDA_CONDITION_RESOURCE");
    let foreign = import_camunda_xml(&gateway(
        r#"<bpmn:conditionExpression xmlns:x="urn:other" xsi:type="x:tOddExpression">${true}</bpmn:conditionExpression>"#,
        &formal("", "${false}"),
    ));
    assert!(foreign.model.is_none());
    only(&foreign, "CAMUNDA_UNSUPPORTED_XSI_TYPE");
}

#[test]
fn a_translator_refusal_is_fatal_for_the_flow_and_carries_the_expr_code() {
    let imported = import_camunda_xml(&gateway(
        &formal("", "${orderService.isValid(order)}"),
        &formal("", "${total > 0}"),
    ));
    assert!(imported.model.is_none());
    let fatal = only(&imported, "CAMUNDA_EXPRESSION_REFUSED");
    assert_eq!(fatal.severity, Some(Severity::Fatal));
    assert_eq!(fatal.element_id.as_deref(), Some("Flow_A"));
    assert!(
        fatal.message.contains("EXPR_UNSUPPORTED_CONSTRUCT"),
        "{}",
        fatal.message
    );
}

#[test]
fn translator_warnings_become_expression_warnings_tied_to_the_flow() {
    let imported = import_camunda_xml(&gateway(
        &formal("", "${amount > 100}"),
        &formal("", "${true}"),
    ));
    let model = assert_clean_import(&imported);
    let warnings: Vec<_> = diagnostics(&imported, "CAMUNDA_EXPRESSION_WARNING");
    assert!(!warnings.is_empty());
    assert!(warnings.iter().all(
        |d| d.severity == Some(Severity::Warning) && d.element_id.as_deref() == Some("Flow_A")
    ));
    let listed = imported
        .translations
        .iter()
        .find(|t| t.element_id == "Flow_A")
        .unwrap();
    assert!(!listed.warnings.is_empty());
    assert!(warnings.iter().any(|d| listed
        .warnings
        .iter()
        .any(|code| d.message.starts_with(code.as_str()))));
    // The constant condition translated without a warning and is still listed.
    let constant = imported
        .translations
        .iter()
        .find(|t| t.element_id == "Flow_B")
        .unwrap();
    assert!(constant.warnings.is_empty());
    assert_eq!(condition_of(model, "Flow_B"), Some("true"));
}

#[test]
fn a_gateway_with_a_warned_condition_gets_one_evaluation_semantics_warning() {
    let imported = import_camunda_xml(&gateway(
        &formal("", "${amount > 100}"),
        &formal("", "${amount &lt;= 100}"),
    ));
    assert_clean_import(&imported);
    let warning = only(&imported, "CAMUNDA_GATEWAY_EVAL_SEMANTICS");
    assert_eq!(warning.severity, Some(Severity::Warning));
    assert_eq!(warning.element_id.as_deref(), Some("Split_1"));
    assert!(warning.message.contains("first true flow"));
}

#[test]
fn every_gateway_with_two_conditions_is_warned_even_when_nothing_else_is() {
    let imported = import_camunda_xml(&gateway(&formal("", "${true}"), &formal("", "${false}")));
    assert_clean_import(&imported);
    assert!(diagnostics(&imported, "CAMUNDA_EXPRESSION_WARNING").is_empty());
    let warning = only(&imported, "CAMUNDA_GATEWAY_EVAL_SEMANTICS");
    assert_eq!(warning.severity, Some(Severity::Warning));
    assert_eq!(warning.element_id.as_deref(), Some("Split_1"));
}

#[test]
fn a_gateway_with_one_condition_and_a_default_is_not_warned() {
    let xml = gateway(&formal("", "${true}"), "").replace(
        "<bpmn:exclusiveGateway id=\"Split_1\"/>",
        "<bpmn:exclusiveGateway id=\"Split_1\" default=\"Flow_B\"/>",
    );
    let imported = import_camunda_xml(&xml);
    assert_clean_import(&imported);
    assert!(diagnostics(&imported, "CAMUNDA_GATEWAY_EVAL_SEMANTICS").is_empty());
}

#[test]
fn a_condition_already_in_cel_passes_through() {
    let cel = import_camunda_xml(&gateway(
        &formal("https://cel.dev/spec", "vars.amount > 100"),
        &formal("https://cel.dev/spec", "vars.amount &lt;= 100"),
    ));
    let model = assert_clean_import(&cel);
    assert_eq!(condition_of(model, "Flow_A"), Some("vars.amount > 100"));
    assert_eq!(condition_of(model, "Flow_B"), Some("vars.amount <= 100"));
    assert!(cel.translations.is_empty());
}

#[test]
fn a_condition_without_a_language_or_wrapper_is_fatal_not_guessed_as_cel() {
    let imported = import_camunda_xml(&gateway(
        &formal("", "vars.amount > 100"),
        &formal("", "${true}"),
    ));
    assert!(imported.model.is_none());
    let fatal = only(&imported, "CAMUNDA_CONDITION_UNTYPED");
    assert_eq!(fatal.severity, Some(Severity::Fatal));
    assert_eq!(fatal.element_id.as_deref(), Some("Flow_A"));
}

#[test]
fn static_prefixed_variables_are_imported_by_name() {
    let imported = import_camunda_xml(&gateway(
        &formal("", "${Static_Region == 'EU'}"),
        &formal("", "${true}"),
    ));
    let model = assert_clean_import(&imported);
    assert!(condition_of(model, "Flow_A")
        .unwrap()
        .contains("vars.Static_Region"));
}

// ----- hostile and foreign input -----

#[test]
fn foreign_namespaces_and_xsi_types_are_reported_not_fatal() {
    let xml = linear(
        r#"<bpmn:userTask id="Task_1" xmlns:zeebe="urn:zeebe" xmlns:m="http://camunda.org/schema/modeler/1.0" m:flag="x" zeebe:retries="3" xsi:type="bpmn:tOtherThing">
          <bpmn:extensionElements><zeebe:taskDefinition type="job"/><camunda:mystery foo="bar"/></bpmn:extensionElements>
        </bpmn:userTask>"#,
    );
    let imported = import_camunda_xml(&xml);
    assert_clean_import(&imported);
    assert_eq!(diagnostics(&imported, "CAMUNDA_FOREIGN_ATTRIBUTE").len(), 2);
    assert_eq!(
        only(&imported, "CAMUNDA_FOREIGN_ELEMENT").severity,
        Some(Severity::Warning)
    );
    only(&imported, "CAMUNDA_UNKNOWN_ELEMENT");
    only(&imported, "CAMUNDA_XSI_TYPE_IGNORED");
}

#[test]
fn unsupported_elements_are_fatal_each_with_their_own_id() {
    let imported = import_camunda_xml(&process(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:businessRuleTask id="Rule_1" camunda:decisionRef="d"/>
        <bpmn:sendTask id="Send_1" camunda:class="x.Y"/>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Rule_1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Rule_1" targetRef="Send_1"/>
        <bpmn:sequenceFlow id="Flow_3" sourceRef="Send_1" targetRef="End_1"/>"#,
    ));
    assert!(imported.model.is_none());
    let mut ids: Vec<_> = diagnostics(&imported, "CAMUNDA_ELEMENT_UNSUPPORTED")
        .iter()
        .map(|d| d.element_id.clone().unwrap())
        .collect();
    ids.sort();
    assert_eq!(ids, ["Rule_1", "Send_1"]);
}

#[test]
fn every_fatal_element_is_listed_in_one_pass() {
    let imported = import_camunda_xml(&process(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:scriptTask id="Js_1" scriptFormat="javascript"><bpmn:script>1</bpmn:script></bpmn:scriptTask>
        <bpmn:scriptTask id="Groovy_1" scriptFormat="groovy"><bpmn:script>1</bpmn:script></bpmn:scriptTask>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Js_1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Js_1" targetRef="Groovy_1"/>
        <bpmn:sequenceFlow id="Flow_3" sourceRef="Groovy_1" targetRef="End_1">
          <bpmn:conditionExpression xsi:type="bpmn:tFormalExpression" language="groovy">true</bpmn:conditionExpression>
        </bpmn:sequenceFlow>"#,
    ));
    assert!(imported.model.is_none());
    let fatal: Vec<_> = imported
        .diagnostics
        .iter()
        .filter(|d| d.fatal)
        .map(|d| d.element_id.clone().unwrap())
        .collect();
    assert_eq!(fatal, ["Js_1", "Groovy_1", "Flow_3"]);
}

#[test]
fn an_overlong_expression_is_refused_without_a_panic() {
    let long = format!("${{{}}}", "a + ".repeat(200) + "a");
    assert!(long.len() > 512);
    let imported = import_camunda_xml(&gateway(&formal("", &long), &formal("", "${true}")));
    assert!(imported.model.is_none());
    let fatal = only(&imported, "CAMUNDA_EXPRESSION_REFUSED");
    assert!(fatal.message.contains("EXPR_TOO_LONG"), "{}", fatal.message);
}

#[test]
fn deep_nesting_is_bounded() {
    let depth = 5_000;
    let xml = format!(
        "{HEAD}<bpmn:process id=\"Process_1\">{}{}</bpmn:process></bpmn:definitions>",
        "<bpmn:subProcess>".repeat(depth),
        "</bpmn:subProcess>".repeat(depth)
    );
    let imported = import_camunda_xml(&xml);
    assert!(imported.model.is_none());
    assert!(
        imported
            .diagnostics
            .iter()
            .any(|d| d.fatal && d.message.contains("nesting")),
        "{:#?}",
        imported.diagnostics
    );
}

#[test]
fn oversize_empty_and_malformed_documents_are_fatal_not_panics() {
    let oversize = format!("{HEAD}<!--{}--></bpmn:definitions>", "x".repeat(600 * 1024));
    assert!(import_camunda_xml(&oversize).model.is_none());
    assert!(import_camunda_xml("").model.is_none());
    assert!(import_camunda_xml("<bpmn:definitions").model.is_none());
    let entity = format!("<!DOCTYPE d [<!ENTITY e \"x\">]>{HEAD}</bpmn:definitions>");
    let imported = import_camunda_xml(&entity);
    assert!(imported.model.is_none() && imported.diagnostics.iter().any(|d| d.fatal));
}

#[test]
fn an_empty_process_is_fatal() {
    let imported = import_camunda_xml(&process(""));
    assert!(imported.model.is_none());
    assert!(imported.diagnostics.iter().any(|d| d.fatal));
}

#[test]
fn duplicate_ids_are_fatal() {
    let imported = import_camunda_xml(&process(
        r#"<bpmn:startEvent id="Start_1"/><bpmn:endEvent id="Start_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Start_1"/>"#,
    ));
    assert!(imported.model.is_none());
    assert!(imported
        .diagnostics
        .iter()
        .any(|d| d.fatal && d.message.contains("duplicate")));
}

#[test]
fn a_flood_of_foreign_attributes_cannot_flood_the_diagnostics() {
    let attributes: String = (0..5_000)
        .map(|i| format!(" camunda:flag{i}=\"x\""))
        .collect();
    let imported = import_camunda_xml(&linear(&format!(
        r#"<bpmn:userTask id="Task_1"{attributes}/>"#
    )));
    assert!(
        imported.diagnostics.len() <= 1002,
        "{}",
        imported.diagnostics.len()
    );
    assert_eq!(
        only(&imported, "CAMUNDA_DIAGNOSTICS_TRUNCATED").severity,
        Some(Severity::Info)
    );
    assert!(imported.model.is_some());
}

#[test]
fn a_fatal_stays_listed_when_the_list_is_full() {
    let attributes: String = (0..2_000)
        .map(|i| format!(" camunda:flag{i}=\"x\""))
        .collect();
    let xml = process(&format!(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:userTask id="Task_1"{attributes}/>
        <bpmn:scriptTask id="Js_1" scriptFormat="javascript"><bpmn:script>1</bpmn:script></bpmn:scriptTask>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Task_1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Task_1" targetRef="Js_1"/>
        <bpmn:sequenceFlow id="Flow_3" sourceRef="Js_1" targetRef="End_1"/>"#
    ));
    let imported = import_camunda_xml(&xml);
    assert!(imported.model.is_none());
    only(&imported, "CAMUNDA_SCRIPT_UNSUPPORTED");
}

#[test]
fn diagram_labels_and_markers_are_dropped_with_one_info() {
    let xml = process(
        r#"<bpmn:startEvent id="Start_1"/><bpmn:exclusiveGateway id="Gw_1"/><bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Gw_1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Gw_1" targetRef="End_1"/>"#,
    )
    .replace(
        "</bpmn:process>",
        r#"</bpmn:process><bpmndi:BPMNDiagram id="D_1"><bpmndi:BPMNPlane id="P_1" bpmnElement="Process_1">
          <bpmndi:BPMNShape id="S_1" bpmnElement="Start_1"><dc:Bounds x="10" y="10" width="36" height="36"/><bpmndi:BPMNLabel><dc:Bounds x="0" y="0" width="10" height="10"/></bpmndi:BPMNLabel></bpmndi:BPMNShape>
          <bpmndi:BPMNShape id="S_2" bpmnElement="Gw_1" isMarkerVisible="true"><dc:Bounds x="100" y="10" width="50" height="50"/></bpmndi:BPMNShape>
          <bpmndi:BPMNShape id="S_3" bpmnElement="End_1"><dc:Bounds x="200" y="10" width="36" height="36"/></bpmndi:BPMNShape>
          <bpmndi:BPMNEdge id="E_1" bpmnElement="Flow_1"><di:waypoint x="46" y="28"/><di:waypoint x="100" y="28"/><bpmndi:BPMNLabel/></bpmndi:BPMNEdge>
        </bpmndi:BPMNPlane></bpmndi:BPMNDiagram>"#,
    );
    let imported = import_camunda_xml(&xml);
    let model = assert_clean_import(&imported);
    assert_eq!(model.diagram.shapes.len(), 3);
    assert_eq!(model.diagram.edges.len(), 1);
    assert_eq!(
        only(&imported, "CAMUNDA_DIAGRAM_DETAIL_IGNORED").severity,
        Some(Severity::Info)
    );
}

// ----- the realistic mixed process and idempotency -----

fn mixed_process() -> String {
    process(
        r#"<bpmn:startEvent id="Start_1" camunda:formKey="start-form"><bpmn:outgoing>Flow_0</bpmn:outgoing></bpmn:startEvent>
        <bpmn:userTask id="Review_1" name="Review order" camunda:assignee="jkowalski" camunda:candidateGroups="sales" camunda:asyncBefore="true">
          <bpmn:documentation>Check the order</bpmn:documentation>
          <bpmn:extensionElements>
            <camunda:taskListener delegateExpression="${notifier}" event="create"/>
            <camunda:inputOutput><camunda:outputParameter name="verdict">${approved}</camunda:outputParameter></camunda:inputOutput>
          </bpmn:extensionElements>
          <bpmn:incoming>Flow_0</bpmn:incoming><bpmn:outgoing>Flow_1</bpmn:outgoing>
        </bpmn:userTask>
        <bpmn:exclusiveGateway id="Split_1"/>
        <bpmn:serviceTask id="Charge_1" name="Charge card" camunda:type="external" camunda:topic="charge-card" camunda:exclusive="false"/>
        <bpmn:serviceTask id="Notify_1" camunda:delegateExpression="${mailer}" camunda:resultVariable="mailId">
          <bpmn:extensionElements><camunda:failedJobRetryTimeCycle>R3/PT5M</camunda:failedJobRetryTimeCycle></bpmn:extensionElements>
        </bpmn:serviceTask>
        <bpmn:serviceTask id="Each_1" camunda:expression="${audit.log(item)}">
          <bpmn:multiInstanceLoopCharacteristics isSequential="true" camunda:collection="${lines}" camunda:elementVariable="item"/>
        </bpmn:serviceTask>
        <bpmn:scriptTask id="Calc_1" scriptFormat="juel" camunda:resultVariable="doubled"><bpmn:script>${total * 2}</bpmn:script></bpmn:scriptTask>
        <bpmn:exclusiveGateway id="Join_1"/>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_0" sourceRef="Start_1" targetRef="Review_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Review_1" targetRef="Split_1"/>
        <bpmn:sequenceFlow id="Flow_High" name="over 100" sourceRef="Split_1" targetRef="Charge_1">
          <bpmn:conditionExpression xsi:type="bpmn:tFormalExpression">${total &gt; 100 &amp;&amp; approved}</bpmn:conditionExpression>
        </bpmn:sequenceFlow>
        <bpmn:sequenceFlow id="Flow_Low" sourceRef="Split_1" targetRef="Calc_1">
          <bpmn:conditionExpression xsi:type="bpmn:tFormalExpression">${total &lt;= 100}</bpmn:conditionExpression>
        </bpmn:sequenceFlow>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Charge_1" targetRef="Notify_1"/>
        <bpmn:sequenceFlow id="Flow_3" sourceRef="Notify_1" targetRef="Each_1"/>
        <bpmn:sequenceFlow id="Flow_4" sourceRef="Each_1" targetRef="Join_1"/>
        <bpmn:sequenceFlow id="Flow_5" sourceRef="Calc_1" targetRef="Join_1"/>
        <bpmn:sequenceFlow id="Flow_6" sourceRef="Join_1" targetRef="End_1"/>"#,
    )
}

#[test]
fn a_realistic_mixed_process_imports_with_the_expected_diagnostics() {
    let imported = import_camunda_xml_resolving(&mixed_process(), &|login| {
        (login == "jkowalski").then(|| "user-7".to_string())
    });
    let model = assert_clean_import(&imported);
    assert_eq!(model.process_name.as_deref(), Some("Order"));
    assert!(matches!(
        &node(model, "Review_1").kind,
        ProcessNodeKind::UserTask { assignee_user_id: Some(id), output_mapping } if id == "user-7" && output_mapping.contains_key("verdict")
    ));
    assert_eq!(
        service_implementation(model, "Charge_1"),
        ServiceImplementation::Unbound {
            origin: "external:charge-card".into()
        }
    );
    assert_eq!(
        service_implementation(model, "Notify_1"),
        ServiceImplementation::Unbound {
            origin: "delegateExpression:${mailer}".into()
        }
    );
    // R3 counts the first run, so the cycle is two automatic retries five minutes apart.
    assert_eq!(retry_of(model, "Notify_1"), Some(ProcessRetryPolicy {
        max_attempts: 2, delay_seconds: 300, backoff: ProcessRetryBackoff::Fixed }));
    assert!(node(model, "Each_1").repeat.is_some());
    assert!(matches!(
        &node(model, "Calc_1").kind,
        ProcessNodeKind::ScriptTask { .. }
    ));
    // Code, owner and grade of every diagnostic the process must raise.
    let expect = [
        ("CAMUNDA_CANDIDATES_SKIPPED", "Review_1", Severity::Warning),
        ("CAMUNDA_FORM_SKIPPED", "Start_1", Severity::Warning),
        ("CAMUNDA_LISTENER_SKIPPED", "Review_1", Severity::Warning),
        (
            "CAMUNDA_GATEWAY_EVAL_SEMANTICS",
            "Split_1",
            Severity::Warning,
        ),
        (
            "CAMUNDA_MULTI_INSTANCE_ELEMENT_VARIABLE",
            "Each_1",
            Severity::Warning,
        ),
        (
            "CAMUNDA_MULTI_INSTANCE_OUTPUT_VARIABLE",
            "Each_1",
            Severity::Warning,
        ),
        ("CAMUNDA_HISTORY_TTL_IGNORED", "Process_1", Severity::Info),
    ];
    assert!(diagnostics(&imported, "CAMUNDA_ASSIGNEE_UNRESOLVED").is_empty());
    for (code, element, severity) in expect {
        let found = only(&imported, code);
        assert_eq!(found.element_id.as_deref(), Some(element), "{code}");
        assert_eq!(found.severity, Some(severity), "{code}");
    }
    let unbound = diagnostics(&imported, "CAMUNDA_SERVICE_UNBOUND");
    let mut owners: Vec<_> = unbound
        .iter()
        .map(|d| d.element_id.clone().unwrap())
        .collect();
    owners.sort();
    assert_eq!(owners, ["Charge_1", "Each_1", "Notify_1"]);
    assert!(unbound
        .iter()
        .all(|d| d.severity == Some(Severity::Warning)));
    let asyncs = diagnostics(&imported, "CAMUNDA_ASYNC_IGNORED");
    let mut owners: Vec<_> = asyncs
        .iter()
        .map(|d| d.element_id.clone().unwrap())
        .collect();
    owners.sort();
    assert_eq!(owners, ["Charge_1", "Review_1"]);
    assert!(asyncs.iter().all(|d| d.severity == Some(Severity::Info)));
    let kinds: Vec<_> = imported
        .translations
        .iter()
        .map(|t| t.kind.as_str())
        .collect();
    for (kind, count) in [
        ("condition", 2),
        ("script", 1),
        ("multi_instance_collection", 1),
        ("output_parameter", 1),
    ] {
        assert_eq!(
            kinds.iter().filter(|k| **k == kind).count(),
            count,
            "{kind} in {kinds:?}"
        );
    }
    // Several unbound steps are listed, none is hidden by the first.
    assert!(validate_model(model).is_err());
}

#[test]
fn camunda_import_export_strict_import_yields_an_equal_model() {
    let imported = import_camunda_xml_resolving(&mixed_process(), &|login| {
        (login == "jkowalski").then(|| "user-7".to_string())
    });
    let model = assert_clean_import(&imported);
    let xml = export_xml(model).expect("an imported draft exports");
    let (reimported, diagnostics) = import_xml(&xml);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(reimported.as_ref(), Some(model));
    // The exported document is TentaFlow's own again: a second round changes nothing.
    assert_eq!(export_xml(reimported.as_ref().unwrap()).unwrap(), xml);
}

#[test]
fn the_exported_draft_re_enters_the_tolerant_path_unchanged() {
    let model = unbound_model();
    let xml = export_xml(&model).unwrap().replacen(
        "<bpmn:definitions",
        "<bpmn:definitions xmlns:camunda=\"http://camunda.org/schema/1.0/bpmn\"",
        1,
    );
    let tolerant = import_camunda_xml(&xml);
    assert!(
        tolerant.diagnostics.is_empty(),
        "{:#?}",
        tolerant.diagnostics
    );
    assert_eq!(tolerant.model.as_ref(), Some(&model));
}

// ----- scope of variables written inside an embedded subprocess -----

fn subprocess_process(inner: &str, after: &str) -> String {
    process(&format!(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:subProcess id="Sub_1">
          <bpmn:startEvent id="SubStart_1"/>
          {inner}
          <bpmn:endEvent id="SubEnd_1"/>
          <bpmn:sequenceFlow id="SubFlow_1" sourceRef="SubStart_1" targetRef="Inner_1"/>
          <bpmn:sequenceFlow id="SubFlow_2" sourceRef="Inner_1" targetRef="SubEnd_1"/>
        </bpmn:subProcess>
        {after}
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Sub_1"/>"#
    ))
}

fn subprocess_mapping(model: &ProcessModel) -> BTreeMap<String, String> {
    match &node(model, "Sub_1").kind {
        ProcessNodeKind::SubProcess { output_mapping, .. } => output_mapping.clone(),
        other => panic!("subprocess expected: {other:?}"),
    }
}

#[test]
fn variables_written_inside_a_subprocess_are_mapped_out_by_name() {
    let imported = import_camunda_xml(&subprocess_process(
        r#"<bpmn:userTask id="Inner_1"><bpmn:extensionElements><camunda:inputOutput><camunda:outputParameter name="verdict">${approved}</camunda:outputParameter></camunda:inputOutput></bpmn:extensionElements></bpmn:userTask>"#,
        r#"<bpmn:endEvent id="End_1"/><bpmn:sequenceFlow id="Flow_2" sourceRef="Sub_1" targetRef="End_1"/>"#,
    ));
    let model = assert_clean_import(&imported);
    let mapping = subprocess_mapping(model);
    assert_eq!(
        mapping.keys().map(String::as_str).collect::<Vec<_>>(),
        ["verdict"]
    );
    assert!(mapping["verdict"].contains("outputs.verdict"));
    assert!(model.variables.contains_key("verdict"));
    assert!(diagnostics(&imported, "CAMUNDA_SUBPROCESS_SCOPE").is_empty());
}

#[test]
fn a_subprocess_with_unknown_steps_warns_that_their_writes_cannot_be_mapped() {
    let imported = import_camunda_xml(&subprocess_process(
        r#"<bpmn:serviceTask id="Inner_1" camunda:delegateExpression="${bean}"/>"#,
        r#"<bpmn:endEvent id="End_1"/><bpmn:sequenceFlow id="Flow_2" sourceRef="Sub_1" targetRef="End_1"/>"#,
    ));
    assert_clean_import(&imported);
    let warning = only(&imported, "CAMUNDA_SUBPROCESS_SCOPE");
    assert_eq!(warning.severity, Some(Severity::Warning));
    assert_eq!(warning.element_id.as_deref(), Some("Sub_1"));
}

#[test]
fn a_native_subprocess_keeps_its_own_mapping() {
    let imported = import_camunda_xml(&subprocess_process(
        r#"<bpmn:userTask id="Inner_1"><bpmn:extensionElements><camunda:inputOutput><camunda:outputParameter name="verdict">${approved}</camunda:outputParameter></camunda:inputOutput></bpmn:extensionElements></bpmn:userTask>"#,
        r#"<bpmn:endEvent id="End_1"/><bpmn:sequenceFlow id="Flow_2" sourceRef="Sub_1" targetRef="End_1"/>"#,
    ));
    let model = assert_clean_import(&imported);
    // Exported, the subprocess is TentaFlow's own: reading it again must not add to it.
    let xml = export_xml(model).unwrap().replacen(
        "<bpmn:definitions",
        "<bpmn:definitions xmlns:camunda=\"http://camunda.org/schema/1.0/bpmn\"",
        1,
    );
    let again = import_camunda_xml(&xml);
    assert!(again.diagnostics.is_empty(), "{:#?}", again.diagnostics);
    assert_eq!(again.model.as_ref(), Some(model));
}

#[tokio::test]
async fn a_value_set_inside_a_subprocess_reaches_the_gateway_after_it_at_runtime() {
    let split = r#"<bpmn:exclusiveGateway id="Split_1"/>
        <bpmn:scriptTask id="Left_1" scriptFormat="juel" camunda:resultVariable="path"><bpmn:script>${'A'}</bpmn:script></bpmn:scriptTask>
        <bpmn:scriptTask id="Right_1" scriptFormat="juel" camunda:resultVariable="path"><bpmn:script>${'B'}</bpmn:script></bpmn:scriptTask>
        <bpmn:exclusiveGateway id="Join_1"/>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Sub_1" targetRef="Split_1"/>
        <bpmn:sequenceFlow id="Flow_A" sourceRef="Split_1" targetRef="Left_1"><bpmn:conditionExpression xsi:type="bpmn:tFormalExpression">${verdict == 'approved'}</bpmn:conditionExpression></bpmn:sequenceFlow>
        <bpmn:sequenceFlow id="Flow_B" sourceRef="Split_1" targetRef="Right_1"><bpmn:conditionExpression xsi:type="bpmn:tFormalExpression">${verdict != 'approved'}</bpmn:conditionExpression></bpmn:sequenceFlow>
        <bpmn:sequenceFlow id="Flow_A2" sourceRef="Left_1" targetRef="Join_1"/>
        <bpmn:sequenceFlow id="Flow_B2" sourceRef="Right_1" targetRef="Join_1"/>
        <bpmn:sequenceFlow id="Flow_E" sourceRef="Join_1" targetRef="End_1"/>"#;
    let xml = subprocess_process(
        r#"<bpmn:scriptTask id="Inner_1" scriptFormat="juel" camunda:resultVariable="verdict"><bpmn:script>${'approved'}</bpmn:script></bpmn:scriptTask>"#,
        split,
    );
    let imported = import_camunda_xml(&xml);
    let model = assert_clean_import(&imported);
    let fixture = Fixture::new();
    let started = start_model(&fixture, model);
    assert_eq!(
        started.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed,
        "{started:#?}"
    );
    assert_eq!(started.variables["verdict"], "approved");
    // The branch that tests `verdict` ran, so the subprocess value crossed its scope.
    assert_eq!(started.variables["path"], "A");

    // Without the generated mapping the gateway cannot see the value at all.
    let mut broken = model.clone();
    if let ProcessNodeKind::SubProcess { output_mapping, .. } = &mut broken
        .nodes
        .iter_mut()
        .find(|n| n.id == "Sub_1")
        .unwrap()
        .kind
    {
        output_mapping.clear();
    }
    let blocked = start_model(&fixture, &broken);
    assert_eq!(blocked.variables["verdict"], serde_json::Value::Null);
    assert_eq!(blocked.variables["path"], "B");
}

// ----- strict reader and native steps -----

fn declared(xml: String) -> String {
    xml.replacen(
        "<bpmn:definitions",
        "<bpmn:definitions xmlns:camunda=\"http://camunda.org/schema/1.0/bpmn\"",
        1,
    )
}

#[test]
fn strict_import_still_refuses_origin_on_a_step_that_is_not_unbound() {
    let xml = export_xml(&unbound_model()).unwrap();
    let base = r#"implementation="unbound" origin="delegateExpression:${bean}""#;
    assert!(xml.contains(base));
    for replacement in [
        r#"flowId="f" origin="x""#,
        r#"implementation="tool" addonId="a" toolName="t" origin="x""#,
        r#"implementation="block" nodeType="n" origin="x""#,
        r#"implementation="agent" agentId="a" maxIterations="3" origin="x""#,
    ] {
        let (model, diagnostics) = import_xml(&xml.replace(base, replacement));
        assert!(model.is_none(), "{replacement}");
        assert!(
            diagnostics[0].fatal && diagnostics[0].severity.is_none(),
            "{replacement}"
        );
    }
}

#[test]
fn a_native_documented_manual_task_reimports_unchanged_when_the_namespace_is_declared() {
    let mut model = starter_model();
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Manual_1".into(),
            name: "Prepare".into(),
            kind: ProcessNodeKind::ManualTask {
                assignee_user_id: None,
                instructions: "Bring the form".into(),
            },
            repeat: None,
            activity_io: None,
        },
    );
    model.sequence_flows[0].target_id = "Manual_1".into();
    model
        .sequence_flows
        .push(tentaflow_protocol::processes::ProcessSequenceFlow {
            id: "Flow_M".into(),
            source_id: "Manual_1".into(),
            target_id: "End_1".into(),
            condition: None,
            call_start_node_id: None,
        });
    let xml = export_xml(&model).unwrap();
    assert!(xml.contains("Bring the form"));
    let (strict, diagnostics) = import_xml(&xml);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    let tolerant = import_camunda_xml(&declared(xml));
    assert!(
        tolerant.diagnostics.is_empty(),
        "{:#?}",
        tolerant.diagnostics
    );
    assert_eq!(tolerant.model, strict);
}

#[test]
fn camunda_attributes_on_a_step_that_already_has_a_tentaflow_definition_are_reported() {
    let service = declared(export_xml(&unbound_model()).unwrap()).replacen(
        "<bpmn:serviceTask ",
        "<bpmn:serviceTask camunda:class=\"x.Y\" camunda:resultVariable=\"r\" ",
        1,
    );
    let imported = import_camunda_xml(&service);
    assert_eq!(imported.model.as_ref(), Some(&unbound_model()));
    let ignored = diagnostics(&imported, "CAMUNDA_NATIVE_STEP_ATTRIBUTE_IGNORED");
    assert_eq!(ignored.len(), 2);
    assert!(ignored.iter().all(
        |d| d.severity == Some(Severity::Warning) && d.element_id.as_deref() == Some("Task_1")
    ));

    let user = import_camunda_xml_resolving(
        &linear(r#"<bpmn:userTask id="Task_1" camunda:assignee="jk"/>"#),
        &|_| Some("user-1".into()),
    );
    let user_xml = declared(export_xml(user.model.as_ref().unwrap()).unwrap()).replacen(
        "<bpmn:userTask ",
        "<bpmn:userTask camunda:assignee=\"other\" ",
        1,
    );
    let again = import_camunda_xml(&user_xml);
    assert_eq!(again.model, user.model);
    assert_eq!(
        only(&again, "CAMUNDA_NATIVE_STEP_ATTRIBUTE_IGNORED")
            .element_id
            .as_deref(),
        Some("Task_1")
    );
}

#[test]
fn script_resource_and_result_variable_on_a_native_script_are_reported() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:scriptTask id="Task_1" scriptFormat="application/vnd.tentaflow.cel" camunda:resultVariable="r" camunda:resource="x"><bpmn:script>vars.a + 1</bpmn:script></bpmn:scriptTask>"#,
    ));
    assert_clean_import(&imported);
    assert_eq!(
        diagnostics(&imported, "CAMUNDA_NATIVE_STEP_ATTRIBUTE_IGNORED").len(),
        2
    );
}

#[test]
fn non_default_activity_attributes_are_warnings_and_defaults_are_info() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:userTask id="Task_1" isForCompensation="true" startQuantity="2" completionQuantity="1"/>"#,
    ));
    assert_clean_import(&imported);
    let lost = diagnostics(&imported, "CAMUNDA_ATTRIBUTE_LOST");
    assert_eq!(lost.len(), 2);
    assert!(lost.iter().all(|d| d.severity == Some(Severity::Warning)));
    let ignored: Vec<_> = diagnostics(&imported, "CAMUNDA_ATTRIBUTE_IGNORED")
        .into_iter()
        .filter(|d| d.element_id.as_deref() == Some("Task_1"))
        .collect();
    assert_eq!(ignored.len(), 1);
    assert_eq!(ignored[0].severity, Some(Severity::Info));
}

#[test]
fn connector_parameters_are_reported_not_merged_into_the_step() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:serviceTask id="Task_1"><bpmn:extensionElements><camunda:connector>
            <camunda:inputOutput><camunda:inputParameter name="url">http://x</camunda:inputParameter></camunda:inputOutput>
            <camunda:connectorId>http-connector</camunda:connectorId>
        </camunda:connector></bpmn:extensionElements></bpmn:serviceTask>"#,
    ));
    let model = assert_clean_import(&imported);
    let ProcessNodeKind::ServiceTask { input_mapping, .. } = &node(model, "Task_1").kind else {
        panic!("service task expected");
    };
    assert!(input_mapping.is_empty());
    assert_eq!(
        only(&imported, "CAMUNDA_CONNECTOR_PARAMETERS_SKIPPED").severity,
        Some(Severity::Warning)
    );
}

#[test]
fn a_foreign_type_on_a_loop_condition_is_fatal() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:userTask id="Task_1"><bpmn:standardLoopCharacteristics testBefore="true" loopMaximum="3"><bpmn:loopCondition xmlns:x="urn:other" xsi:type="x:tOdd" language="https://cel.dev/spec">true</bpmn:loopCondition></bpmn:standardLoopCharacteristics></bpmn:userTask>"#,
    ));
    assert!(imported.model.is_none());
    assert!(diagnostics(&imported, "CAMUNDA_UNSUPPORTED_XSI_TYPE")
        .iter()
        .any(|d| d.fatal && d.element_id.as_deref() == Some("Task_1")));
}

// ----- bounds -----

#[test]
fn translations_are_capped_like_diagnostics() {
    let parameters: String = (0..1_100)
        .map(|i| format!(r#"<camunda:inputParameter name="p{i}">v{i}</camunda:inputParameter>"#))
        .collect();
    let imported = import_camunda_xml(&linear(&format!(
        r#"<bpmn:serviceTask id="Task_1" camunda:class="x.Y"><bpmn:extensionElements><camunda:inputOutput>{parameters}</camunda:inputOutput></bpmn:extensionElements></bpmn:serviceTask>"#
    )));
    assert_eq!(imported.translations.len(), 1000);
    only(&imported, "CAMUNDA_DIAGNOSTICS_TRUNCATED");
}

#[test]
fn diagnostic_messages_are_bounded() {
    let long = "x".repeat(5_000);
    let imported = import_camunda_xml(&linear(&format!(
        r#"<bpmn:userTask id="Task_1" camunda:mystery="{long}"/>"#
    )));
    assert!(imported
        .diagnostics
        .iter()
        .all(|d| d.message.chars().count() <= 1001));
}

#[test]
fn login_lookups_are_capped_per_import_and_the_rest_stay_unresolved() {
    let tasks: String = (0..250)
        .map(|i| format!(r#"<bpmn:userTask id="T{i}" camunda:assignee="user{i}"/>"#))
        .collect();
    let flows: String = (0..250)
        .map(|i| {
            let from = if i == 0 {
                "Start_1".to_string()
            } else {
                format!("T{}", i - 1)
            };
            format!(r#"<bpmn:sequenceFlow id="F{i}" sourceRef="{from}" targetRef="T{i}"/>"#)
        })
        .collect();
    let xml = process(&format!(
        r#"<bpmn:startEvent id="Start_1"/>{tasks}<bpmn:endEvent id="End_1"/>{flows}<bpmn:sequenceFlow id="F_end" sourceRef="T249" targetRef="End_1"/>"#
    ));
    let calls = std::cell::Cell::new(0);
    let imported = import_camunda_xml_resolving(&xml, &|login| {
        calls.set(calls.get() + 1);
        Some(format!("id-{login}"))
    });
    // A model holds at most 128 nodes, so the document is refused afterwards;
    // the lookups, which happen while reading, are what is bounded.
    assert!(imported.model.is_none());
    assert_eq!(calls.get(), 200);
    assert_eq!(
        diagnostics(&imported, "CAMUNDA_ASSIGNEE_UNRESOLVED").len(),
        50
    );
}

#[test]
fn nesting_just_under_the_cap_reaches_the_rewriter_and_the_strict_reader_without_overflow() {
    // Each level carries an id, so the document passes the reader and the
    // rewrite, per-node check and strict builder all recurse to the bottom.
    let depth = 28;
    let open: String = (0..depth)
        .map(|i| format!("<bpmn:subProcess id=\"S{i}\">"))
        .collect();
    let close = "</bpmn:subProcess>".repeat(depth);
    let xml = format!(
        "{HEAD}<bpmn:process id=\"Process_1\">{open}<bpmn:startEvent id=\"Deep_1\"/>{close}</bpmn:process></bpmn:definitions>"
    );
    let imported = import_camunda_xml(&xml);
    assert!(imported.model.is_none());
    assert!(imported.diagnostics.iter().any(|d| d.fatal));
    assert!(
        !imported
            .diagnostics
            .iter()
            .any(|d| d.message.contains("nesting deeper")),
        "{:#?}",
        imported.diagnostics.first()
    );
}

// ----- hand-in of parent values, nested scopes, gateway race, depth, element variable -----

#[test]
fn a_synthesized_subprocess_hands_the_parent_value_in_and_copies_it_out_plainly() {
    let imported = import_camunda_xml(&subprocess_process(
        r#"<bpmn:scriptTask id="Inner_1" scriptFormat="juel" camunda:resultVariable="amount"><bpmn:script>${5}</bpmn:script></bpmn:scriptTask>"#,
        r#"<bpmn:endEvent id="End_1"/><bpmn:sequenceFlow id="Flow_2" sourceRef="Sub_1" targetRef="End_1"/>"#,
    ));
    let model = assert_clean_import(&imported);
    let ProcessNodeKind::SubProcess {
        input_mapping,
        output_mapping,
        ..
    } = &node(model, "Sub_1").kind
    else {
        panic!("subprocess expected");
    };
    assert_eq!(input_mapping["amount"], "vars.amount");
    assert_eq!(output_mapping["amount"], "outputs.amount");
}

#[test]
fn a_subprocess_writing_more_names_than_a_mapping_holds_is_fatal_and_named() {
    let task = |id: &str, from: usize| {
        let params: String = (from..from + 70)
            .map(|i| format!(r#"<camunda:outputParameter name="v{i}">x</camunda:outputParameter>"#))
            .collect();
        format!(
            r#"<bpmn:userTask id="{id}"><bpmn:extensionElements><camunda:inputOutput>{params}</camunda:inputOutput></bpmn:extensionElements></bpmn:userTask>"#
        )
    };
    let xml = process(&format!(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:subProcess id="Sub_1">
          <bpmn:startEvent id="SubStart_1"/>{}{}<bpmn:endEvent id="SubEnd_1"/>
          <bpmn:sequenceFlow id="SF_1" sourceRef="SubStart_1" targetRef="A_1"/>
          <bpmn:sequenceFlow id="SF_2" sourceRef="A_1" targetRef="B_1"/>
          <bpmn:sequenceFlow id="SF_3" sourceRef="B_1" targetRef="SubEnd_1"/>
        </bpmn:subProcess>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Sub_1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Sub_1" targetRef="End_1"/>"#,
        task("A_1", 0),
        task("B_1", 70)
    ));
    let imported = import_camunda_xml(&xml);
    assert!(imported.model.is_none());
    let fatal = only(&imported, "CAMUNDA_MODEL_INVALID");
    assert_eq!(fatal.element_id.as_deref(), Some("Sub_1"));
    assert!(fatal.message.contains("Sub_1") && fatal.message.contains("128"));
}

#[tokio::test]
async fn the_body_reads_the_parent_value_until_it_overwrites_it_and_nested_scopes_copy_out() {
    // `amount` is 500 before the subprocess. The body's gateway reads it, a step
    // then sets it to 5, and a nested subprocess sets `deep`. The gateway after
    // the subprocess sees the new `amount` and the nested `deep`.
    let xml = process(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:scriptTask id="Seed_1" scriptFormat="juel" camunda:resultVariable="amount"><bpmn:script>${500}</bpmn:script></bpmn:scriptTask>
        <bpmn:subProcess id="Sub_1">
          <bpmn:startEvent id="SubStart_1"/>
          <bpmn:exclusiveGateway id="Gate_1"/>
          <bpmn:scriptTask id="Mark_1" scriptFormat="juel" camunda:resultVariable="sawOld"><bpmn:script>${true}</bpmn:script></bpmn:scriptTask>
          <bpmn:scriptTask id="Set_1" scriptFormat="juel" camunda:resultVariable="amount"><bpmn:script>${5}</bpmn:script></bpmn:scriptTask>
          <bpmn:scriptTask id="Low_1" scriptFormat="juel" camunda:resultVariable="sawLow"><bpmn:script>${true}</bpmn:script></bpmn:scriptTask>
          <bpmn:exclusiveGateway id="GateJoin_1"/>
          <bpmn:subProcess id="Sub_2">
            <bpmn:startEvent id="InStart_1"/>
            <bpmn:scriptTask id="Deep_1" scriptFormat="juel" camunda:resultVariable="deep"><bpmn:script>${'x'}</bpmn:script></bpmn:scriptTask>
            <bpmn:endEvent id="InEnd_1"/>
            <bpmn:sequenceFlow id="IF_1" sourceRef="InStart_1" targetRef="Deep_1"/>
            <bpmn:sequenceFlow id="IF_2" sourceRef="Deep_1" targetRef="InEnd_1"/>
          </bpmn:subProcess>
          <bpmn:endEvent id="SubEnd_1"/>
          <bpmn:sequenceFlow id="SF_0" sourceRef="SubStart_1" targetRef="Gate_1"/>
          <bpmn:sequenceFlow id="SF_Hi" sourceRef="Gate_1" targetRef="Mark_1"><bpmn:conditionExpression xsi:type="bpmn:tFormalExpression">${amount &gt; 100}</bpmn:conditionExpression></bpmn:sequenceFlow>
          <bpmn:sequenceFlow id="SF_Lo" sourceRef="Gate_1" targetRef="Low_1"><bpmn:conditionExpression xsi:type="bpmn:tFormalExpression">${amount &lt;= 100}</bpmn:conditionExpression></bpmn:sequenceFlow>
          <bpmn:sequenceFlow id="SF_Mark" sourceRef="Mark_1" targetRef="Set_1"/>
          <bpmn:sequenceFlow id="SF_J1" sourceRef="Set_1" targetRef="GateJoin_1"/>
          <bpmn:sequenceFlow id="SF_J2" sourceRef="Low_1" targetRef="GateJoin_1"/>
          <bpmn:sequenceFlow id="SF_Sub2" sourceRef="GateJoin_1" targetRef="Sub_2"/>
          <bpmn:sequenceFlow id="SF_End" sourceRef="Sub_2" targetRef="SubEnd_1"/>
        </bpmn:subProcess>
        <bpmn:exclusiveGateway id="After_1"/>
        <bpmn:scriptTask id="New_1" scriptFormat="juel" camunda:resultVariable="path"><bpmn:script>${'new'}</bpmn:script></bpmn:scriptTask>
        <bpmn:scriptTask id="Old_1" scriptFormat="juel" camunda:resultVariable="path"><bpmn:script>${'old'}</bpmn:script></bpmn:scriptTask>
        <bpmn:exclusiveGateway id="Join_1"/>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Seed_1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Seed_1" targetRef="Sub_1"/>
        <bpmn:sequenceFlow id="Flow_3" sourceRef="Sub_1" targetRef="After_1"/>
        <bpmn:sequenceFlow id="Flow_New" sourceRef="After_1" targetRef="New_1"><bpmn:conditionExpression xsi:type="bpmn:tFormalExpression">${amount &lt; 100 &amp;&amp; deep == 'x'}</bpmn:conditionExpression></bpmn:sequenceFlow>
        <bpmn:sequenceFlow id="Flow_Old" sourceRef="After_1" targetRef="Old_1"><bpmn:conditionExpression xsi:type="bpmn:tFormalExpression">${amount &gt;= 100 || deep != 'x'}</bpmn:conditionExpression></bpmn:sequenceFlow>
        <bpmn:sequenceFlow id="Flow_4" sourceRef="New_1" targetRef="Join_1"/>
        <bpmn:sequenceFlow id="Flow_5" sourceRef="Old_1" targetRef="Join_1"/>
        <bpmn:sequenceFlow id="Flow_6" sourceRef="Join_1" targetRef="End_1"/>"#,
    );
    let imported = import_camunda_xml(&xml);
    let model = assert_clean_import(&imported);
    let fixture = Fixture::new();
    let done = start_model(&fixture, model);
    assert_eq!(
        done.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed,
        "{done:#?}"
    );
    // The gateway inside the body read the parent's 500, not a shadowing null.
    assert_eq!(done.variables["sawOld"], true);
    assert_eq!(done.variables["sawLow"], serde_json::Value::Null);
    assert_eq!(done.variables["amount"], 5);
    assert_eq!(done.variables["deep"], "x");
    assert_eq!(done.variables["path"], "new");
}

#[test]
fn a_gateway_race_counts_an_unconditional_non_default_flow() {
    let imported = import_camunda_xml(&gateway(&formal("", "${a > 1}"), ""));
    let warning = only(&imported, "CAMUNDA_GATEWAY_EVAL_SEMANTICS");
    assert_eq!(warning.element_id.as_deref(), Some("Split_1"));
    // The default flow stays out of the count.
    let with_default = gateway(&formal("", "${a > 1}"), "").replace(
        "<bpmn:exclusiveGateway id=\"Split_1\"/>",
        "<bpmn:exclusiveGateway id=\"Split_1\" default=\"Flow_B\"/>",
    );
    assert!(diagnostics(
        &import_camunda_xml(&with_default),
        "CAMUNDA_GATEWAY_EVAL_SEMANTICS"
    )
    .is_empty());
}

#[test]
fn deeply_nested_documents_are_refused_on_the_strict_path_without_a_stack_overflow() {
    for depth in [70usize, 200, 25_000] {
        let xml = format!(
            "{HEAD}{}{}</bpmn:definitions>",
            "<bpmn:x>".repeat(depth),
            "</bpmn:x>".repeat(depth)
        );
        let (model, diagnostics) = import_xml(&xml);
        assert!(model.is_none() && diagnostics[0].fatal, "{depth}");
        assert!(
            diagnostics[0].message.contains("nesting"),
            "{depth}: {}",
            diagnostics[0].message
        );
        let tolerant = import_camunda_xml(&xml);
        assert!(tolerant.model.is_none());
    }
}

#[test]
fn the_strict_path_survives_the_deepest_nesting_it_still_reads() {
    let depth = 40;
    let open: String = (0..depth)
        .map(|i| format!("<bpmn:subProcess id=\"S{i}\">"))
        .collect();
    let close = "</bpmn:subProcess>".repeat(depth);
    let xml = format!(
        "{HEAD}<bpmn:process id=\"Process_1\">{open}<bpmn:startEvent id=\"Deep_1\"/>{close}</bpmn:process></bpmn:definitions>"
    );
    let (model, diagnostics) = import_xml(&xml);
    assert!(model.is_none() && !diagnostics.is_empty());
}

#[test]
fn the_element_variable_check_reads_identifiers_not_text() {
    let activity = |collection: &str, variable: &str, parameter: &str| {
        linear(&format!(
            r#"<bpmn:userTask id="Task_1"><bpmn:extensionElements><camunda:inputOutput><camunda:outputParameter name="seen">{parameter}</camunda:outputParameter></camunda:inputOutput></bpmn:extensionElements><bpmn:multiInstanceLoopCharacteristics camunda:collection="{collection}" camunda:elementVariable="{variable}"/></bpmn:userTask>"#
        ))
    };
    // The collection's own source is not a read of the element variable.
    let own = import_camunda_xml(&activity("${lines}", "lines", "${total}"));
    assert_clean_import(&own);
    // Neither is a string literal nor a property name.
    let literal = import_camunda_xml(&activity("${items}", "item", "${kind == 'item'}"));
    assert_clean_import(&literal);
    let property = import_camunda_xml(&activity("${items}", "item", "${order.item}"));
    assert_clean_import(&property);
    // A real read is.
    let read = import_camunda_xml(&activity("${items}", "item", "${item.name}"));
    assert!(read.model.is_none());
    only(&read, "CAMUNDA_MULTI_INSTANCE_ELEMENT_VARIABLE");
}

// ----- timers, labels, positions, ids, names -----

fn timer_task(event: &str, flows: &str) -> String {
    process(&format!(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:userTask id="Task_1" name="Review"/>
        <bpmn:endEvent id="End_1"/>
        <bpmn:endEvent id="EndB_1"/>
        {event}
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Task_1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Task_1" targetRef="End_1"/>
        {flows}"#
    ))
}

fn boundary_timer(rule: &str) -> String {
    timer_task(
        &format!(
            r#"<bpmn:boundaryEvent id="Timer_1" name="Too slow" attachedToRef="Task_1"><bpmn:timerEventDefinition id="TimerEventDefinition_9">{rule}</bpmn:timerEventDefinition></bpmn:boundaryEvent>"#
        ),
        r#"<bpmn:sequenceFlow id="Flow_3" sourceRef="Timer_1" targetRef="EndB_1"/>"#,
    )
}

#[test]
fn a_boundary_timer_gets_a_defaulted_zone_and_a_warning_naming_the_timer() {
    let imported = import_camunda_xml(&boundary_timer(
        r#"<bpmn:timeDuration xsi:type="bpmn:tFormalExpression">PT1H</bpmn:timeDuration>"#,
    ));
    let model = assert_clean_import(&imported);
    assert_eq!(model.timer_timezone.as_deref(), Some("UTC"));
    let warning = only(&imported, "CAMUNDA_TIMER_TIMEZONE_DEFAULTED");
    // A duration in hours fires at the same instant in any zone.
    assert_eq!(warning.severity, Some(Severity::Info));
    assert_eq!(warning.element_id.as_deref(), Some("Timer_1"));
    assert_eq!(warning.element_name.as_deref(), Some("Too slow"));
    assert!(warning.message.contains("UTC"));
    // The draft exports and reads back.
    let xml = export_xml(model).unwrap();
    let (again, diagnostics) = import_xml(&xml);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(again.as_ref(), Some(model));
    let declared_again = import_camunda_xml(&declared(xml));
    assert!(
        declared_again.diagnostics.is_empty(),
        "{:#?}",
        declared_again.diagnostics
    );
}

#[test]
fn start_and_intermediate_timers_are_imported_too() {
    let cycle = import_camunda_xml(&process(
        r#"<bpmn:startEvent id="Start_1"><bpmn:timerEventDefinition><bpmn:timeCycle xsi:type="bpmn:tFormalExpression">R3/PT600S</bpmn:timeCycle></bpmn:timerEventDefinition></bpmn:startEvent>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="End_1"/>"#,
    ));
    let model = assert_clean_import(&cycle);
    assert!(matches!(
        &node(model, "Start_1").kind,
        ProcessNodeKind::TimerStart { .. }
    ));
    only(&cycle, "CAMUNDA_TIMER_TIMEZONE_DEFAULTED");
    let date = import_camunda_xml(&process(
        r#"<bpmn:startEvent id="Start_1"><bpmn:timerEventDefinition><bpmn:timeDate xsi:type="bpmn:tFormalExpression">2030-01-01T10:00:00Z</bpmn:timeDate></bpmn:timerEventDefinition></bpmn:startEvent>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="End_1"/>"#,
    ));
    assert_clean_import(&date);
    let wait = import_camunda_xml(&process(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:intermediateCatchEvent id="Wait_1"><bpmn:timerEventDefinition><bpmn:timeDuration xsi:type="bpmn:tFormalExpression">P1DT2H</bpmn:timeDuration></bpmn:timerEventDefinition></bpmn:intermediateCatchEvent>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Wait_1"/>
        <bpmn:sequenceFlow id="Flow_2" sourceRef="Wait_1" targetRef="End_1"/>"#,
    ));
    let model = assert_clean_import(&wait);
    assert!(matches!(
        &node(model, "Wait_1").kind,
        ProcessNodeKind::TimerCatch { .. }
    ));
    assert_eq!(
        only(&wait, "CAMUNDA_TIMER_TIMEZONE_DEFAULTED")
            .element_id
            .as_deref(),
        Some("Wait_1")
    );
}

#[test]
fn a_document_without_timers_gets_no_zone() {
    let imported = import_camunda_xml(&linear(r#"<bpmn:userTask id="Task_1"/>"#));
    assert_eq!(assert_clean_import(&imported).timer_timezone, None);
    assert!(diagnostics(&imported, "CAMUNDA_TIMER_TIMEZONE_DEFAULTED").is_empty());
}

#[test]
fn a_timer_value_that_cannot_be_mapped_is_fatal_and_names_the_timer() {
    for rule in [
        r#"<bpmn:timeCycle xsi:type="bpmn:tFormalExpression">R3/PT10M</bpmn:timeCycle>"#,
        r#"<bpmn:timeDuration xsi:type="bpmn:tFormalExpression">${deadline}</bpmn:timeDuration>"#,
        r#"<bpmn:timeCycle xsi:type="bpmn:tFormalExpression">0 0 12 * * ?</bpmn:timeCycle>"#,
    ] {
        let imported = import_camunda_xml(&boundary_timer(rule));
        assert!(imported.model.is_none(), "{rule}");
        let fatal = only(&imported, "CAMUNDA_TIMER_UNSUPPORTED");
        assert_eq!(fatal.severity, Some(Severity::Fatal));
        assert_eq!(fatal.element_id.as_deref(), Some("Timer_1"));
        assert_eq!(fatal.element_name.as_deref(), Some("Too slow"));
        assert!(fatal.message.contains("Supported"), "{}", fatal.message);
    }
}

#[test]
fn an_event_subprocess_is_refused_naming_it() {
    let imported = import_camunda_xml(&process(
        r#"<bpmn:startEvent id="Start_1"/>
        <bpmn:subProcess id="Ev_1" triggeredByEvent="true"><bpmn:startEvent id="EvStart_1"/><bpmn:endEvent id="EvEnd_1"/><bpmn:sequenceFlow id="EF_1" sourceRef="EvStart_1" targetRef="EvEnd_1"/></bpmn:subProcess>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="End_1"/>"#,
    ));
    assert!(imported.model.is_none());
    assert_eq!(
        only(&imported, "CAMUNDA_ELEMENT_UNSUPPORTED")
            .element_id
            .as_deref(),
        Some("Ev_1")
    );
}

#[test]
fn the_tool_attributes_are_silent_and_a_flow_label_gets_one_readable_info() {
    let imported = import_camunda_xml(&gateway(&formal("", "${true}"), &formal("", "${false}")));
    assert!(
        imported
            .diagnostics
            .iter()
            .all(|d| !d.message.contains("exporter")),
        "{:#?}",
        imported.diagnostics
    );
    let label = only(&imported, "CAMUNDA_FLOW_LABEL_IGNORED");
    assert_eq!(label.severity, Some(Severity::Info));
    assert_eq!(label.element_id.as_deref(), Some("Flow_A"));
    assert!(label.message.contains("yes"));
    assert!(diagnostics(&imported, "CAMUNDA_ATTRIBUTE_IGNORED")
        .iter()
        .all(|d| !d.message.contains("name=")));
}

#[test]
fn a_syntax_error_carries_line_and_column() {
    let xml = format!("{HEAD}\n<bpmn:process id=\"P\" bad>\n</bpmn:process></bpmn:definitions>");
    let imported = import_camunda_xml(&xml);
    assert!(imported.model.is_none());
    let fatal = imported.diagnostics.iter().find(|d| d.fatal).unwrap();
    assert!(
        fatal.message.contains("(line 3, column"),
        "{}",
        fatal.message
    );
    // So does a refusal of a single element.
    let element = import_camunda_xml(&process(
        "<bpmn:startEvent id=\"Start_1\"/>\n<bpmn:businessRuleTask id=\"Rule_1\"/>\n<bpmn:endEvent id=\"End_1\"/>",
    ));
    let fatal = only(&element, "CAMUNDA_ELEMENT_UNSUPPORTED");
    assert!(
        fatal.message.contains("(line 3, column"),
        "{}",
        fatal.message
    );
}

#[test]
fn an_invalid_id_names_the_rule_it_breaks_and_its_element() {
    for (id, expected) in [
        ("1abc", "must start with a letter"),
        ("a b", "invalid characters"),
        (&"x".repeat(130), "at most 128"),
    ] {
        let xml = process(&format!(
            r#"<bpmn:startEvent id="Start_1"/><bpmn:userTask id="{id}" name="Check"/><bpmn:endEvent id="End_1"/>
            <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="{id}"/>
            <bpmn:sequenceFlow id="Flow_2" sourceRef="{id}" targetRef="End_1"/>"#
        ));
        let imported = import_camunda_xml(&xml);
        assert!(imported.model.is_none(), "{id}");
        let fatal = only(&imported, "CAMUNDA_ID_INVALID");
        assert_eq!(fatal.element_id.as_deref(), Some(id), "{id}");
        assert_eq!(fatal.element_name.as_deref(), Some("Check"));
        assert!(fatal.message.contains(expected), "{}", fatal.message);
    }
}

#[test]
fn a_refused_element_keeps_its_name_for_the_listing() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:scriptTask id="Task_1" name="Calculate tax" scriptFormat="javascript"><bpmn:script>1</bpmn:script></bpmn:scriptTask>"#,
    ));
    let fatal = only(&imported, "CAMUNDA_SCRIPT_UNSUPPORTED");
    assert_eq!(fatal.element_name.as_deref(), Some("Calculate tax"));
}

// ----- zones per process, dates, interruptions, names -----

fn two_pools(first_timer: bool, second_timer: bool) -> String {
    let body = |suffix: &str, timer: bool| {
        if timer {
            format!(
                r#"<bpmn:startEvent id="Start_{suffix}"/>
                <bpmn:intermediateCatchEvent id="Wait_{suffix}"><bpmn:timerEventDefinition><bpmn:timeDuration xsi:type="bpmn:tFormalExpression">PT30M</bpmn:timeDuration></bpmn:timerEventDefinition></bpmn:intermediateCatchEvent>
                <bpmn:endEvent id="End_{suffix}"/>
                <bpmn:sequenceFlow id="F1_{suffix}" sourceRef="Start_{suffix}" targetRef="Wait_{suffix}"/>
                <bpmn:sequenceFlow id="F2_{suffix}" sourceRef="Wait_{suffix}" targetRef="End_{suffix}"/>"#
            )
        } else {
            format!(
                r#"<bpmn:startEvent id="Start_{suffix}"/><bpmn:endEvent id="End_{suffix}"/>
                <bpmn:sequenceFlow id="F1_{suffix}" sourceRef="Start_{suffix}" targetRef="End_{suffix}"/>"#
            )
        }
    };
    format!(
        r#"{HEAD}<bpmn:collaboration id="Collab_1">
          <bpmn:participant id="Part_1" name="Sales" processRef="Process_1"/>
          <bpmn:participant id="Part_2" name="Billing" processRef="Process_2"/>
        </bpmn:collaboration>
        <bpmn:process id="Process_1" isExecutable="true">{}</bpmn:process>
        <bpmn:process id="Process_2" isExecutable="true">{}</bpmn:process>
        </bpmn:definitions>"#,
        body("1", first_timer),
        body("2", second_timer)
    )
}

#[test]
fn every_process_with_timers_gets_its_own_zone_and_none_without() {
    let both = import_camunda_xml(&two_pools(true, true));
    let model = assert_clean_import(&both);
    assert_eq!(model.timer_timezone.as_deref(), Some("UTC"));
    assert_eq!(
        model.additional_processes[0].timer_timezone.as_deref(),
        Some("UTC")
    );
    let mut owners: Vec<_> = diagnostics(&both, "CAMUNDA_TIMER_TIMEZONE_DEFAULTED")
        .iter()
        .map(|d| d.element_id.clone().unwrap())
        .collect();
    owners.sort();
    assert_eq!(owners, ["Wait_1", "Wait_2"]);

    let second_only = import_camunda_xml(&two_pools(false, true));
    let model = assert_clean_import(&second_only);
    assert_eq!(model.timer_timezone, None);
    assert_eq!(
        model.additional_processes[0].timer_timezone.as_deref(),
        Some("UTC")
    );

    let first_only = import_camunda_xml(&two_pools(true, false));
    let model = assert_clean_import(&first_only);
    assert_eq!(model.timer_timezone.as_deref(), Some("UTC"));
    assert_eq!(model.additional_processes[0].timer_timezone, None);
    only(&first_only, "CAMUNDA_TIMER_TIMEZONE_DEFAULTED");
}

#[test]
fn a_date_the_model_would_refuse_is_refused_naming_the_timer() {
    let date = |rule: &str| {
        process(&format!(
            r#"<bpmn:startEvent id="Start_1"><bpmn:timerEventDefinition>{rule}</bpmn:timerEventDefinition></bpmn:startEvent>
            <bpmn:endEvent id="End_1"/>
            <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="End_1"/>"#
        ))
    };
    for rule in [
        r#"<bpmn:timeDate xsi:type="bpmn:tFormalExpression">${dueDate}</bpmn:timeDate>"#,
        r#"<bpmn:timeDate xsi:type="bpmn:tFormalExpression">2030-01-01T10:00:00</bpmn:timeDate>"#,
        r#"<bpmn:timeDate xsi:type="bpmn:tFormalExpression">${deadline}</bpmn:timeDate><bpmn:timeCycle xsi:type="bpmn:tFormalExpression">R3/PT60S</bpmn:timeCycle>"#,
    ] {
        let imported = import_camunda_xml(&date(rule));
        assert!(imported.model.is_none(), "{rule}");
        let fatal = only(&imported, "CAMUNDA_TIMER_UNSUPPORTED");
        assert_eq!(fatal.element_id.as_deref(), Some("Start_1"), "{rule}");
        assert!(
            diagnostics(&imported, "CAMUNDA_TIMER_TIMEZONE_DEFAULTED").is_empty(),
            "{rule}"
        );
        assert!(
            diagnostics(&imported, "CAMUNDA_MODEL_INVALID").is_empty(),
            "{rule}"
        );
    }
}

#[test]
fn the_zone_warning_is_graded_by_whether_the_zone_matters() {
    let with = |rule: &str| {
        let imported = import_camunda_xml(&boundary_timer(rule));
        assert_clean_import(&imported);
        imported
    };
    let hours =
        with(r#"<bpmn:timeDuration xsi:type="bpmn:tFormalExpression">PT90M</bpmn:timeDuration>"#);
    assert_eq!(
        only(&hours, "CAMUNDA_TIMER_TIMEZONE_DEFAULTED").severity,
        Some(Severity::Info)
    );
    assert!(diagnostics(&hours, "CAMUNDA_TIMER_DAY_DURATION").is_empty());
    let cycle = import_camunda_xml(&process(
        r#"<bpmn:startEvent id="Start_1"><bpmn:timerEventDefinition><bpmn:timeCycle xsi:type="bpmn:tFormalExpression">R3/PT600S</bpmn:timeCycle></bpmn:timerEventDefinition></bpmn:startEvent>
        <bpmn:endEvent id="End_1"/>
        <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="End_1"/>"#,
    ));
    assert_clean_import(&cycle);
    assert_eq!(
        only(&cycle, "CAMUNDA_TIMER_TIMEZONE_DEFAULTED").severity,
        Some(Severity::Info)
    );
    let days =
        with(r#"<bpmn:timeDuration xsi:type="bpmn:tFormalExpression">P1DT2H</bpmn:timeDuration>"#);
    let day = only(&days, "CAMUNDA_TIMER_DAY_DURATION");
    assert_eq!(day.severity, Some(Severity::Warning));
    assert_eq!(day.element_id.as_deref(), Some("Timer_1"));
    assert!(day.message.contains("P1DT2H") && day.message.contains("86400"));
}

#[test]
fn a_subprocess_that_copies_variables_out_and_can_be_interrupted_is_warned() {
    let sub = |cancel: &str| {
        process(&format!(
            r#"<bpmn:startEvent id="Start_1"/>
            <bpmn:subProcess id="Sub_1">
              <bpmn:startEvent id="SubStart_1"/>
              <bpmn:scriptTask id="Inner_1" scriptFormat="juel" camunda:resultVariable="partial"><bpmn:script>${{1}}</bpmn:script></bpmn:scriptTask>
              <bpmn:endEvent id="SubEnd_1"/>
              <bpmn:sequenceFlow id="SF_1" sourceRef="SubStart_1" targetRef="Inner_1"/>
              <bpmn:sequenceFlow id="SF_2" sourceRef="Inner_1" targetRef="SubEnd_1"/>
            </bpmn:subProcess>
            <bpmn:boundaryEvent id="Guard_1" attachedToRef="Sub_1"{cancel}><bpmn:timerEventDefinition><bpmn:timeDuration xsi:type="bpmn:tFormalExpression">PT1H</bpmn:timeDuration></bpmn:timerEventDefinition></bpmn:boundaryEvent>
            <bpmn:endEvent id="End_1"/>
            <bpmn:endEvent id="EndB_1"/>
            <bpmn:sequenceFlow id="Flow_1" sourceRef="Start_1" targetRef="Sub_1"/>
            <bpmn:sequenceFlow id="Flow_2" sourceRef="Sub_1" targetRef="End_1"/>
            <bpmn:sequenceFlow id="Flow_3" sourceRef="Guard_1" targetRef="EndB_1"/>"#
        ))
    };
    let interrupting = import_camunda_xml(&sub(""));
    assert_clean_import(&interrupting);
    let warning = only(&interrupting, "CAMUNDA_SUBPROCESS_INTERRUPTED_SCOPE");
    assert_eq!(warning.severity, Some(Severity::Warning));
    assert_eq!(warning.element_id.as_deref(), Some("Sub_1"));
    let non_interrupting = import_camunda_xml(&sub(r#" cancelActivity="false""#));
    assert!(diagnostics(&non_interrupting, "CAMUNDA_SUBPROCESS_INTERRUPTED_SCOPE").is_empty());
}

#[test]
fn element_names_are_cleaned_and_capped_at_two_hundred_characters() {
    let name = format!("a\u{202E}b&#10;c{}", "x".repeat(300));
    let imported = import_camunda_xml(&linear(&format!(
        r#"<bpmn:scriptTask id="Task_1" name="{name}" scriptFormat="javascript"><bpmn:script>1</bpmn:script></bpmn:scriptTask>"#
    )));
    let label = only(&imported, "CAMUNDA_SCRIPT_UNSUPPORTED")
        .element_name
        .clone()
        .unwrap();
    assert_eq!(label.chars().count(), 200);
    assert!(
        label.starts_with("abcxxx") && label.ends_with('…'),
        "{label}"
    );
    assert!(!label.chars().any(|c| c.is_control() || c == '\u{202E}'));
}

#[test]
fn a_leading_byte_order_mark_is_not_a_column() {
    let xml = "\u{feff}<?xml version=\"1.0\"?><definitions bad>";
    let imported = import_camunda_xml(xml);
    let fatal = imported.diagnostics.iter().find(|d| d.fatal).unwrap();
    assert!(
        fatal.message.contains("(line 1, column 22)"),
        "{}",
        fatal.message
    );
}

// ----- failedJobRetryTimeCycle -> retry policy -----

/// A job-backed task (asyncBefore), the only kind whose retry cycle Camunda runs.
fn cycle_task(cycle: &str) -> String {
    linear(&format!(
        r#"<bpmn:serviceTask id="Task_1" camunda:class="x.Y" camunda:asyncBefore="true"><bpmn:extensionElements><camunda:failedJobRetryTimeCycle>{cycle}</camunda:failedJobRetryTimeCycle></bpmn:extensionElements></bpmn:serviceTask>"#
    ))
}

#[test]
fn a_retry_cycle_that_maps_exactly_becomes_the_policy_without_a_diagnostic() {
    let policy = |max_attempts, delay_seconds, backoff| Some(ProcessRetryPolicy { max_attempts, delay_seconds, backoff });
    let grown = |factor_percent, max_delay_seconds| ProcessRetryBackoff::Exponential { factor_percent, max_delay_seconds };
    for (cycle, expected) in [
        ("R3/PT10M", policy(2, 600, ProcessRetryBackoff::Fixed)),
        ("R11/PT1H30M", policy(10, 5400, ProcessRetryBackoff::Fixed)),
        ("R2/P1D", policy(1, 86_400, ProcessRetryBackoff::Fixed)),
        ("R4/PT30S,PT30S,PT30S", policy(3, 30, ProcessRetryBackoff::Fixed)),
        // Camunda 7.7+: without R<n> every interval is one retry.
        ("PT10M", policy(1, 600, ProcessRetryBackoff::Fixed)),
        ("PT5M,PT5M,PT5M", policy(3, 300, ProcessRetryBackoff::Fixed)),
        ("PT10S,PT20S,PT40S", policy(3, 10, grown(200, 40))),
        // The list repeats its last interval, so a cycle that grows and then holds is exponential with a cap.
        ("R5/PT1M,PT5M,PT15M", policy(4, 60, grown(500, 900))),
        ("R4/PT10S,PT20S,PT40S", policy(3, 10, grown(200, 40))),
        // R1 is one run and no retry: the default, not a policy.
        ("R1/PT10M", None),
    ] {
        let imported = import_camunda_xml(&cycle_task(cycle));
        let model = assert_clean_import(&imported);
        assert_eq!(retry_of(model, "Task_1"), expected, "{cycle}");
        assert!(diagnostics(&imported, "CAMUNDA_RETRY_CYCLE_SKIPPED").is_empty(), "{cycle}");
        assert!(diagnostics(&imported, "CAMUNDA_RETRY_CYCLE_APPROXIMATED").is_empty(), "{cycle}");
    }
}

#[test]
fn a_list_of_intervals_that_is_not_a_steady_series_is_approximated_and_says_so() {
    let imported = import_camunda_xml(&cycle_task("R4/PT1M,PT2M,PT4M"));
    let model = assert_clean_import(&imported);
    // 60, 120, 240 is exactly 200 percent: exact. Break the series.
    assert!(diagnostics(&imported, "CAMUNDA_RETRY_CYCLE_APPROXIMATED").is_empty());
    assert!(retry_of(model, "Task_1").is_some());

    let imported = import_camunda_xml(&cycle_task("R4/PT1M,PT2M,PT7M"));
    let model = assert_clean_import(&imported);
    let policy = retry_of(model, "Task_1").expect("the closest policy is imported");
    assert_eq!(policy.max_attempts, 3);
    let note = only(&imported, "CAMUNDA_RETRY_CYCLE_APPROXIMATED");
    assert_eq!(note.severity, Some(Severity::Warning));
    assert!(note.message.contains("R4/PT1M,PT2M,PT7M"), "{}", note.message);
    assert_eq!(note.element_id.as_deref(), Some("Task_1"));
}

#[test]
fn a_plain_list_of_intervals_has_one_retry_per_interval() {
    let imported = import_camunda_xml(&cycle_task("PT10M,PT17M,PT20M"));
    let model = assert_clean_import(&imported);
    let policy = retry_of(model, "Task_1").expect("the three intervals are three retries");
    // 600, 1020, 1200: a factor of 170 percent that reaches its cap on the third wait, so it is exact.
    assert_eq!(
        policy,
        ProcessRetryPolicy {
            max_attempts: 3,
            delay_seconds: 600,
            backoff: ProcessRetryBackoff::Exponential { factor_percent: 170, max_delay_seconds: 1200 },
        }
    );
    assert!(diagnostics(&imported, "CAMUNDA_RETRY_CYCLE_APPROXIMATED").is_empty());
    assert!(diagnostics(&imported, "CAMUNDA_RETRY_CYCLE_SKIPPED").is_empty());

    let imported = import_camunda_xml(&cycle_task("PT10M,PT20M,PT25M,PT40M"));
    let model = assert_clean_import(&imported);
    assert_eq!(retry_of(model, "Task_1").map(|p| p.max_attempts), Some(4));
    only(&imported, "CAMUNDA_RETRY_CYCLE_APPROXIMATED");
}

#[test]
fn cutting_a_cycle_down_to_what_a_step_can_have_is_an_approximation_not_an_exact_map() {
    let long_list = (1..=12).map(|n| format!("PT{n}M")).collect::<Vec<_>>().join(",");
    for (cycle, attempts) in [
        // More runs than a step has retries: capped at ten.
        ("R12/PT10M", 10),
        ("R30/PT10M", 10),
        // More intervals than R<n> has retries: the extra ones are dropped.
        ("R3/PT10M,PT10M,PT10M", 2),
        ("R2/PT1M,PT2M", 1),
        (long_list.as_str(), 10),
    ] {
        let imported = import_camunda_xml(&cycle_task(cycle));
        let model = assert_clean_import(&imported);
        let policy = retry_of(model, "Task_1").unwrap_or_else(|| panic!("{cycle}: no policy"));
        assert_eq!(policy.max_attempts, attempts, "{cycle}");
        let note = only(&imported, "CAMUNDA_RETRY_CYCLE_APPROXIMATED");
        assert!(note.message.contains(&clip_for_test(cycle)), "{cycle}: {}", note.message);
        assert!(diagnostics(&imported, "CAMUNDA_RETRY_CYCLE_SKIPPED").is_empty(), "{cycle}");
    }
}

fn clip_for_test(cycle: &str) -> String {
    cycle.chars().take(20).collect()
}

#[test]
fn a_series_that_does_not_start_rising_is_approximated_not_dropped() {
    for (cycle, delay_seconds, grows) in [
        ("R4/PT10S,PT10S,PT20S", 10, true),
        ("R3/PT10M,PT5M", 600, false),
        ("R4/PT40S,PT20S,PT10S", 40, false),
        ("R3/PT1S,PT10H", 1, true),
    ] {
        let imported = import_camunda_xml(&cycle_task(cycle));
        let model = assert_clean_import(&imported);
        let policy = retry_of(model, "Task_1").unwrap_or_else(|| panic!("{cycle}: no policy"));
        assert_eq!(policy.delay_seconds, delay_seconds, "{cycle}");
        assert_eq!(
            matches!(policy.backoff, ProcessRetryBackoff::Exponential { .. }),
            grows,
            "{cycle}: {policy:?}"
        );
        only(&imported, "CAMUNDA_RETRY_CYCLE_APPROXIMATED");
        assert!(diagnostics(&imported, "CAMUNDA_RETRY_CYCLE_SKIPPED").is_empty(), "{cycle}");
    }
}

#[test]
fn a_retry_cycle_on_a_task_camunda_would_not_retry_is_warned_about() {
    let cycle = "<bpmn:extensionElements><camunda:failedJobRetryTimeCycle>R3/PT10M</camunda:failedJobRetryTimeCycle></bpmn:extensionElements>";
    for (attributes, warned) in [
        (r#"camunda:class="x.Y""#, true),
        (r#"camunda:type="external" camunda:topic="t" camunda:asyncBefore="true""#, true),
        (r#"camunda:class="x.Y" camunda:asyncBefore="false""#, true),
        (r#"camunda:class="x.Y" camunda:asyncBefore="true""#, false),
        (r#"camunda:class="x.Y" camunda:asyncAfter="true""#, false),
    ] {
        let imported = import_camunda_xml(&linear(&format!(
            r#"<bpmn:serviceTask id="Task_1" {attributes}>{cycle}</bpmn:serviceTask>"#
        )));
        let model = assert_clean_import(&imported);
        let found = diagnostics(&imported, "CAMUNDA_RETRY_CYCLE_NO_JOB");
        assert_eq!(found.len(), usize::from(warned), "{attributes}");
        if let Some(warning) = found.first() {
            assert_eq!(warning.severity, Some(Severity::Warning));
            assert_eq!(warning.element_id.as_deref(), Some("Task_1"));
        }
        assert!(retry_of(model, "Task_1").is_some(), "the policy is still imported: {attributes}");
    }
}

#[test]
fn a_retry_cycle_that_cannot_be_mapped_keeps_its_warning_and_imports_no_policy() {
    for cycle in [
        "R3/2024-01-01T00:00:00Z/PT10M",
        "R0/PT10M",
        "R3/PT0S",
        "R3/P2D",
        "R3/P1M",
        "R3/${retryInterval}",
        "R3/",
        "PT10M,",
        "${retryInterval}",
    ] {
        let imported = import_camunda_xml(&cycle_task(cycle));
        let model = assert_clean_import(&imported);
        assert_eq!(retry_of(model, "Task_1"), None, "{cycle}");
        let warning = only(&imported, "CAMUNDA_RETRY_CYCLE_SKIPPED");
        assert_eq!(warning.severity, Some(Severity::Warning), "{cycle}");
        assert!(warning.message.contains(cycle), "{cycle}: {}", warning.message);
        assert!(!warning.message.contains("do not exist yet"), "{}", warning.message);
    }
}

#[test]
fn a_retry_cycle_on_a_step_that_has_no_policy_is_skipped_with_a_warning() {
    let imported = import_camunda_xml(&linear(
        r#"<bpmn:scriptTask id="Task_1" scriptFormat="juel"><bpmn:extensionElements><camunda:failedJobRetryTimeCycle>R3/PT10M</camunda:failedJobRetryTimeCycle></bpmn:extensionElements><bpmn:script>${1}</bpmn:script></bpmn:scriptTask>"#,
    ));
    let warning = only(&imported, "CAMUNDA_RETRY_CYCLE_SKIPPED");
    assert_eq!(warning.element_id.as_deref(), Some("Task_1"));
}

#[test]
fn an_imported_policy_survives_strict_export_and_reimport() {
    let imported = import_camunda_xml(&cycle_task("R5/PT1M,PT5M,PT15M"));
    let model = assert_clean_import(&imported).clone();
    let xml = export_xml(&model).expect("a draft with an unbound step exports");
    assert!(xml.contains(r#"<tentaflow:retry attempts="4" delaySeconds="60" backoff="exponential" factorPercent="500" maxDelaySeconds="900"/>"#), "{xml}");
    let (reimported, diagnostics) = import_xml(&xml);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(reimported.as_ref(), Some(&model));
    assert_eq!(export_xml(reimported.as_ref().unwrap()).unwrap(), xml, "the round trip is stable");
}

#[test]
fn strict_import_rejects_a_retry_element_that_contradicts_itself() {
    let model = assert_clean_import(&import_camunda_xml(&cycle_task("R3/PT10M"))).clone();
    let xml = export_xml(&model).unwrap();
    for bad in [
        r#"<tentaflow:retry attempts="2" delaySeconds="600" backoff="fixed" factorPercent="200" maxDelaySeconds="900"/>"#,
        r#"<tentaflow:retry attempts="2" delaySeconds="600" backoff="exponential"/>"#,
        r#"<tentaflow:retry attempts="two" delaySeconds="600" backoff="fixed"/>"#,
        r#"<tentaflow:retry attempts="2" delaySeconds="600" backoff="sideways"/>"#,
    ] {
        let broken = xml.replace(r#"<tentaflow:retry attempts="2" delaySeconds="600" backoff="fixed"/>"#, bad);
        assert_ne!(broken, xml);
        assert!(import_xml(&broken).0.is_none(), "{bad}");
    }
}
