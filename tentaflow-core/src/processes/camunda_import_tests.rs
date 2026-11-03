// ===== File: processes/camunda_import_tests.rs — tolerant Camunda 7 import: one fixture per mapping row, hostile input and round trips =====

use std::collections::BTreeMap;

use tentaflow_protocol::processes::{
    ProcessDiagnostic, ProcessDiagnosticSeverity as Severity, ProcessModel,
    ProcessMultiInstanceInput, ProcessMultiInstanceMode, ProcessNode, ProcessNodeKind,
    ProcessRepeatSpec, ServiceImplementation,
};

use super::bpmn::{
    declares_camunda_namespace, export_xml, import_camunda_xml, import_camunda_xml_resolving,
    import_document, import_xml, CamundaImport,
};
use super::model::{all_nodes, starter_model, validate_draft, validate_model};

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
fn an_expression_assignee_is_never_looked_up() {
    let xml = linear(r#"<bpmn:userTask id="Task_1" camunda:assignee="${initiator}"/>"#);
    let imported = import_camunda_xml_resolving(&xml, &|_| panic!("an expression is not a login"));
    only(&imported, "CAMUNDA_ASSIGNEE_UNRESOLVED");
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
        r#"<bpmn:userTask id="Task_1"><bpmn:multiInstanceLoopCharacteristics><bpmn:loopCardinality>3</bpmn:loopCardinality><bpmn:completionCondition xsi:type="bpmn:tFormalExpression">${nrOfCompletedInstances &gt;= 2}</bpmn:completionCondition></bpmn:multiInstanceLoopCharacteristics></bpmn:userTask>"#,
    ));
    let model = assert_clean_import(&counted);
    assert!(matches!(
        &node(model, "Task_1").repeat,
        Some(ProcessRepeatSpec::MultiInstance {
            input: ProcessMultiInstanceInput::Cardinality { count: 3 },
            ..
        })
    ));
    // Dropping the early-exit rule changes behavior, so it is not silent.
    only(&counted, "CAMUNDA_MULTI_INSTANCE_COMPLETION_SKIPPED");
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
          <bpmn:extensionElements><camunda:failedJobRetryTimeCycle>R3/PT10M</camunda:failedJobRetryTimeCycle></bpmn:extensionElements>
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
    assert!(retry.message.contains("R3/PT10M"));
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
fn a_gateway_without_warned_conditions_gets_no_evaluation_semantics_warning() {
    let imported = import_camunda_xml(&gateway(&formal("", "${true}"), &formal("", "${false}")));
    assert_clean_import(&imported);
    assert!(diagnostics(&imported, "CAMUNDA_GATEWAY_EVAL_SEMANTICS").is_empty());
    assert!(diagnostics(&imported, "CAMUNDA_EXPRESSION_WARNING").is_empty());
}

#[test]
fn a_condition_already_in_cel_passes_through() {
    let cel = import_camunda_xml(&gateway(
        &formal("https://cel.dev/spec", "vars.amount > 100"),
        "<bpmn:conditionExpression>vars.amount &lt;= 100</bpmn:conditionExpression>",
    ));
    let model = assert_clean_import(&cel);
    assert_eq!(condition_of(model, "Flow_A"), Some("vars.amount > 100"));
    assert_eq!(condition_of(model, "Flow_B"), Some("vars.amount <= 100"));
    assert!(cel.translations.is_empty());
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
        <bpmn:exclusiveGateway id="Split_1" default="Flow_Low"/>
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
        <bpmn:sequenceFlow id="Flow_Low" sourceRef="Split_1" targetRef="Calc_1"/>
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
    assert!(node(model, "Each_1").repeat.is_some());
    assert!(matches!(
        &node(model, "Calc_1").kind,
        ProcessNodeKind::ScriptTask { .. }
    ));
    for code in [
        "CAMUNDA_CANDIDATES_SKIPPED",
        "CAMUNDA_FORM_SKIPPED",
        "CAMUNDA_LISTENER_SKIPPED",
        "CAMUNDA_RETRY_CYCLE_SKIPPED",
        "CAMUNDA_GATEWAY_EVAL_SEMANTICS",
        "CAMUNDA_MULTI_INSTANCE_ELEMENT_VARIABLE",
    ] {
        only(&imported, code);
    }
    assert_eq!(diagnostics(&imported, "CAMUNDA_SERVICE_UNBOUND").len(), 3);
    assert_eq!(diagnostics(&imported, "CAMUNDA_ASYNC_IGNORED").len(), 2);
    only(&imported, "CAMUNDA_HISTORY_TTL_IGNORED");
    let kinds: Vec<_> = imported
        .translations
        .iter()
        .map(|t| t.kind.as_str())
        .collect();
    for kind in [
        "condition",
        "script",
        "multi_instance_collection",
        "output_parameter",
    ] {
        assert_eq!(
            kinds.iter().filter(|k| **k == kind).count(),
            1,
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
