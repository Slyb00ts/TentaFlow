// =============================================================================
// File: tests/ui_v1_roundtrip.rs — typed ui_v1 bindings → canonical CBOR →
// spec decoder round-trip + byte-for-byte parity with the C# SDK goldens.
//
// Golden hex vectors are copied verbatim from
// tentaflow-sdk-dotnet/TentaFlow.Sdk.Tests/golden/vectors.txt (produced by
// the tentaflow-sdk-spec encoders, verified against C# in GoldenWireTests).
// Matching them here proves a Rust addon emits the exact bytes the host
// validator, the JS sdk-runtime and the C# SDK agree on.
// =============================================================================

use tentaflow_addon_sdk::ui_v1::{
    self as ui, backend, backend_kv, bound, lit, local, state_path,
};
use tentaflow_addon_sdk::ui_v1::{
    BindRef, CachePolicy, CborMap, Component, EventKind, FailurePolicy, Handler, HandlerMap,
    Heading, LocalAction, PatchOp, PatchOpKind, PathSegment, SlotContent, SlotDecl,
    SlotDefault, SlotSemantics, SlotVisibility, StateEntry, StatePatch, StatePath, TestId,
    Text, TextStyle, Tone, UiPayload, Value,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn encode<T: minicbor::Encode<()>>(v: &T) -> Vec<u8> {
    minicbor::to_vec(v).expect("encode")
}

/// The C# GoldenWireTests `SampleText()` value, built with the Rust bindings.
fn sample_text() -> Component {
    Text {
        content: lit("Hello"),
        style: TextStyle::Body,
        tone: Some(Tone::Primary),
        align: None,
        wrap: None,
        max_lines: Some(3),
        format: None,
        streaming: None,
    }
    .into_component("txt-1")
    .expect("Text into_component")
}

/// The C# GoldenWireTests `SampleHeading()` value: bound content, a backend
/// click handler with static params and a test id.
fn sample_heading() -> Component {
    let mut c = Heading {
        content: BindRef::Bound(StatePath::new(vec![
            PathSegment::Key("title".into()),
            PathSegment::Index(2),
        ])),
        level: 2,
        tone: None,
        align: None,
    }
    .into_component("hd-1")
    .expect("Heading into_component");
    c.handlers = Some(HandlerMap(vec![(
        EventKind::Click,
        Handler::Backend {
            action_id: "do-it".into(),
            // Kept in canonical (bytewise) key order so the decoded form is
            // field-identical; the encoder sorts either way.
            params: CborMap(vec![
                ("aa".into(), Value::Text("x".into())),
                ("zz".into(), Value::U64(9)),
            ]),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    c.test_id = Some(TestId::new("hd-test").expect("test id"));
    c
}

#[test]
fn text_component_matches_csharp_golden() {
    let bytes = encode(&sample_text());
    assert_eq!(
        hex(&bytes),
        "a30019020101657478742d3102a400a2646b696e64676c69746572616c6576616c75656548656c6c6f0164626f647902677072696d6172790503"
    );
    tentaflow_sdk_spec::validate_canonical(&bytes).expect("canonical CBOR");

    // Round-trip: bytes → Component envelope → typed Text.
    let decoded: Component = minicbor::decode(&bytes).expect("decode Component");
    assert_eq!(decoded.tag, Text::TAG);
    assert_eq!(decoded.id, "txt-1");
    let back = Text::try_from_component(&decoded).expect("typed decode");
    assert_eq!(back.content, lit("Hello"));
    assert_eq!(back.style, TextStyle::Body);
    assert_eq!(back.tone, Some(Tone::Primary));
    assert_eq!(back.max_lines, Some(3));
}

#[test]
fn heading_with_handler_matches_csharp_golden() {
    let bytes = encode(&sample_heading());
    assert_eq!(
        hex(&bytes),
        "a500190202016468642d3102a200a2646b696e6465626f756e64647061746882a2646b696e64636b65796576616c7565657469746c65a2646b696e6465696e6465786576616c756502010203a165636c69636ba4646b696e64676261636b656e6466706172616d73a26261616178627a7a0969616374696f6e5f696465646f2d69746a6f6e5f6661696c757265a1646b696e6465746f617374076768642d74657374"
    );
    tentaflow_sdk_spec::validate_canonical(&bytes).expect("canonical CBOR");

    let decoded: Component = minicbor::decode(&bytes).expect("decode Component");
    assert_eq!(decoded.tag, Heading::TAG);
    assert_eq!(decoded, sample_heading(), "bit-identical round-trip");
    let handlers = decoded.handlers.expect("handlers present");
    assert_eq!(handlers.0.len(), 1);
    assert_eq!(handlers.0[0].0, EventKind::Click);
    match &handlers.0[0].1 {
        Handler::Backend { action_id, on_failure, .. } => {
            assert_eq!(action_id, "do-it");
            assert_eq!(*on_failure, FailurePolicy::Toast);
        }
        other => panic!("expected backend handler, got {other:?}"),
    }
}

#[test]
fn slot_content_payload_matches_csharp_golden() {
    let payload = UiPayload::SlotContent(SlotContent {
        addon_id: "hello-dotnet".into(),
        panel_id: "main".into(),
        panel_epoch: 7,
        slot_id: "content".into(),
        fragment: sample_text(),
        state_overlay: Some(vec![StateEntry {
            path: state_path("ready"),
            value: Value::Bool(true),
        }]),
    });
    let bytes = encode(&payload);
    assert_eq!(
        hex(&bytes),
        "82190110a6006c68656c6c6f2d646f746e657401646d61696e02070367636f6e74656e7404a30019020101657478742d3102a400a2646b696e64676c69746572616c6576616c75656548656c6c6f0164626f647902677072696d61727905030581a20081a2646b696e64636b65796576616c756565726561647901f5"
    );
    tentaflow_sdk_spec::validate_canonical(&bytes).expect("canonical CBOR");

    let back: UiPayload = minicbor::decode(&bytes).expect("decode UiPayload");
    assert_eq!(back, payload, "UiPayload round-trip");
}

#[test]
fn state_patch_payload_matches_csharp_golden() {
    let payload = UiPayload::StatePatch(StatePatch {
        addon_id: "hello-dotnet".into(),
        panel_id: "main".into(),
        panel_epoch: 7,
        base_revision: 3,
        new_revision: 4,
        ops: vec![
            PatchOp {
                path: state_path("count"),
                op: PatchOpKind::Increment { delta: -2 },
            },
            PatchOp {
                path: state_path("items"),
                op: PatchOpKind::AppendArray { value: Value::Text("new".into()) },
            },
            PatchOp {
                path: state_path("tmp"),
                op: PatchOpKind::Delete,
            },
        ],
    });
    let bytes = encode(&payload);
    assert_eq!(
        hex(&bytes),
        "82190121a6006c68656c6c6f2d646f746e657401646d61696e0207030304040583a20081a2646b696e64636b65796576616c756565636f756e7401a2646b696e6469696e6372656d656e746564656c746121a20081a2646b696e64636b65796576616c7565656974656d7301a2646b696e646c617070656e645f61727261796576616c7565636e6577a20081a2646b696e64636b65796576616c756563746d7001a1646b696e646664656c657465"
    );
    tentaflow_sdk_spec::validate_canonical(&bytes).expect("canonical CBOR");

    let back: UiPayload = minicbor::decode(&bytes).expect("decode UiPayload");
    assert_eq!(back, payload, "UiPayload round-trip");
}

#[test]
fn panel_shell_built_from_typed_bindings_round_trips() {
    let layout = ui::Stack {
        gap: ui::Spacing::Md,
        align: ui::FlexAlign::Stretch,
        children: vec![sample_text(), sample_heading()],
        padding: None,
        justify: None,
        style: None,
        responsive: None,
    }
    .into_component("root")
    .expect("Stack into_component");

    let payload = UiPayload::PanelShell(ui::PanelShell {
        addon_id: "notes".into(),
        panel_id: "main".into(),
        panel_epoch: 1,
        layout,
        slots: vec![SlotDecl {
            id: "content".into(),
            semantics: SlotSemantics::MainContent,
            default_state: SlotDefault::Loading,
            cache_policy: CachePolicy::TtlSeconds { value: 60 },
            visibility: SlotVisibility::Always,
            max_payload_bytes: None,
        }],
        initial_state: vec![StateEntry {
            path: state_path("count"),
            value: Value::U64(42),
        }],
        initial_commands: vec![],
    });

    let bytes = encode(&payload);
    tentaflow_sdk_spec::validate_canonical(&bytes).expect("canonical CBOR");
    let back: UiPayload = minicbor::decode(&bytes).expect("decode UiPayload");
    assert_eq!(back, payload);
    // Re-encode must be bit-identical (canonical form is unique).
    assert_eq!(encode(&back), bytes);
}

#[test]
fn handler_builders_produce_spec_shapes() {
    let h = backend(EventKind::Click, "save");
    assert_eq!(h.0.len(), 1);
    assert!(matches!(
        &h.0[0],
        (EventKind::Click, Handler::Backend { action_id, optimistic: None, on_failure: FailurePolicy::Toast, .. })
            if action_id == "save"
    ));

    let h = backend_kv(EventKind::Change, "set_config", "auto_save");
    match &h.0[0].1 {
        Handler::Backend { params, .. } => {
            assert_eq!(params.0, vec![("key".to_string(), Value::Text("auto_save".into()))]);
        }
        other => panic!("expected backend handler, got {other:?}"),
    }

    let h = local(EventKind::Click, LocalAction::Toggle { path: state_path("open") });
    assert!(matches!(&h.0[0].1, Handler::Local(LocalAction::Toggle { .. })));
    // Builder output must encode canonically like any hand-built HandlerMap.
    tentaflow_sdk_spec::validate_canonical(&encode(&h)).expect("canonical CBOR");
}

#[test]
fn state_path_and_bind_helpers() {
    assert_eq!(
        state_path("user.name"),
        StatePath::new(vec![
            PathSegment::Key("user".into()),
            PathSegment::Key("name".into()),
        ])
    );
    assert_eq!(lit("x"), BindRef::Literal(Value::Text("x".into())));
    assert_eq!(
        bound("items"),
        BindRef::Bound(StatePath::new(vec![PathSegment::Key("items".into())]))
    );
}
