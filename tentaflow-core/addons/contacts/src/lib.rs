// =============================================================================
// File: addons/contacts/src/lib.rs
// Contacts addon — manages companies, persons, employments and relationship maps.
// Provides LLM tools, flow blocks, and CBOR UI panels via ui_render_cbor.
// =============================================================================

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use serde_json::Value as JsonValue;

use tentaflow_sdk_spec::protocol::ui::bind::{BindRef, PathSegment, StatePath};
use tentaflow_sdk_spec::protocol::ui::component::{Component, HandlerMap};
use tentaflow_sdk_spec::protocol::ui::a11y::EventKind;
use tentaflow_sdk_spec::protocol::ui::data::{Heading, Text};
use tentaflow_sdk_spec::protocol::ui::handler::{FailurePolicy, Handler};
use tentaflow_sdk_spec::protocol::ui::inline::NavTab;
use tentaflow_sdk_spec::protocol::ui::layout::{NavTabs, Stack};
use tentaflow_sdk_spec::protocol::ui::molecules::Inspector;
use tentaflow_sdk_spec::protocol::ui::panel::PanelShell;
use tentaflow_sdk_spec::protocol::ui::patch::{PatchOp, PatchOpKind};
use tentaflow_sdk_spec::protocol::ui::slot::{
    CachePolicy, SlotDecl, SlotDefault, SlotSemantics, SlotVisibility, StateEntry,
};
use tentaflow_sdk_spec::protocol::ui::slot_msg::SlotContent;
use tentaflow_sdk_spec::protocol::ui::state::StatePatch;
use tentaflow_sdk_spec::protocol::ui::tokens::{FlexAlign, NavTabsVariant, Spacing, TextStyle, Tone};
use tentaflow_sdk_spec::protocol::ui::ui_payload::UiPayload;
use tentaflow_sdk_spec::protocol::control::CborMap;
use tentaflow_sdk_spec::protocol::value::Value as CborValue;

// =============================================================================
// Host function imports
// =============================================================================

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn ui_render_cbor(cbor_ptr: i32, cbor_len: i32) -> i32;
    fn log_info(msg_ptr: i32, msg_len: i32) -> i32;
    fn log_warn(msg_ptr: i32, msg_len: i32) -> i32;
    fn tool_register(def_ptr: i32, def_len: i32) -> i32;
    fn ui_notify(
        title_ptr: i32, title_len: i32,
        body_ptr: i32, body_len: i32,
        level_ptr: i32, level_len: i32,
    ) -> i32;
    fn http_request(
        req_ptr: i32, req_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn sql_exec_v1(
        query_ptr: i32, query_len: i32,
        params_json_ptr: i32, params_json_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn sql_query_v1(
        query_ptr: i32, query_len: i32,
        params_json_ptr: i32, params_json_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn sql_query_one_v1(
        query_ptr: i32, query_len: i32,
        params_json_ptr: i32, params_json_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn llm_generate(
        prompt_ptr: i32, prompt_len: i32,
        model_ptr: i32, model_len: i32,
        options_ptr: i32, options_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
}

// =============================================================================
// Constants
// =============================================================================

const ADDON_ID: &str = "contacts";
const PANEL_ID: &str = "main";
const SLOT_ID: &str = "content";
const BASE_API: &str = "https://wl-api.mf.gov.pl/api/search";
const DEFAULT_LIMIT: i64 = 25;
const MAX_LIMIT: i64 = 100;
const SQL_BUF_SIZE: usize = 65536;
const HTTP_BUF_SIZE: usize = 65536;
const LLM_BUF_SIZE: usize = 16384;

// =============================================================================
// Mutable state
// =============================================================================

static mut PANEL_EPOCH: u64 = 1;
static mut STATE_REVISION: u64 = 0;

// =============================================================================
// Lifecycle exports
// =============================================================================

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log("contacts addon started");
    register_contacts_tools();
    // The shell is NOT rendered here: on_start does not receive the
    // host-assigned panel epoch, so a shell emitted now would carry the default
    // epoch and be rejected on any session whose epoch advanced past 1. The
    // host calls on_panel_open (with the authoritative epoch) on every open,
    // including cold starts, so the shell is rendered there exactly once.
    0
}

/// Canonical render entry: the host invokes this on every panel open (cold and
/// warm) with the session-assigned epoch. Rendering here — never in on_start —
/// guarantees the PanelShell carries the epoch the host expects.
#[no_mangle]
pub extern "C" fn on_panel_open(panel_id_ptr: i32, panel_id_len: i32, epoch: i64) -> i32 {
    let panel_id = read_guest_string(panel_id_ptr, panel_id_len);
    if panel_id != PANEL_ID {
        return 0;
    }
    unsafe {
        PANEL_EPOCH = epoch as u64;
        STATE_REVISION = 0;
    }
    send_panel_shell();
    send_tab_content("all");
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn on_event(_event_ptr: i32, _event_len: i32) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn on_request(
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let input_json = read_guest_string(input_ptr, input_len);
    let request: JsonValue = match serde_json::from_str(&input_json) {
        Ok(v) => v,
        Err(e) => {
            return write_response(
                out_ptr, out_cap, out_len_ptr,
                &json_error(format!("Invalid request JSON: {}", e)),
            );
        }
    };

    let tool_name = request.get("tool").and_then(JsonValue::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or_else(|| serde_json::json!({}));
    let result = if let Some(block_type) = tool_name.strip_prefix("block.") {
        handle_flow_block(block_type, &params)
    } else {
        handle_tool(tool_name, &params)
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let layout = core::alloc::Layout::from_size_align(size as usize, 8).unwrap();
    unsafe { alloc::alloc::alloc(layout) as i32 }
}

#[no_mangle]
pub extern "C" fn dealloc(ptr: i32, size: i32) {
    let layout = core::alloc::Layout::from_size_align(size as usize, 8).unwrap();
    unsafe { alloc::alloc::dealloc(ptr as *mut u8, layout) }
}

// =============================================================================
// UI — CBOR panel shell + slot content
// =============================================================================

fn send_ui(payload: &UiPayload) -> i32 {
    let mut buf = Vec::with_capacity(1024);
    minicbor::encode(payload, &mut buf).unwrap();
    unsafe { ui_render_cbor(buf.as_ptr() as i32, buf.len() as i32) }
}

fn send_panel_shell() {
    let epoch = unsafe { PANEL_EPOCH };

    let mut nav_tabs = NavTabs {
        items: vec![
            nav_tab("all", "Wszystkie"),
            nav_tab("companies", "Firmy"),
            nav_tab("persons", "Osoby"),
            nav_tab("relationship-map", "Mapy relacji"),
            nav_tab("smart-lists", "Smart lists"),
        ],
        active_id: bound("active_tab"),
        variant: NavTabsVariant::Underlined,
        scroll_overflow: true,
    }
    .into_component("nav-tabs")
    .expect("NavTabs encode");
    nav_tabs.handlers = Some(HandlerMap(vec![(
        EventKind::Select,
        Handler::Backend {
            action_id: "panel-navigate".into(),
            params: CborMap(vec![]),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));

    let content_host = Inspector {
        title: lit("Kontakty"),
        content_slot: SLOT_ID.into(),
        actions: vec![],
        tabs: None,
        collapsible: false,
    }
    .into_component("content-host")
    .expect("Inspector encode");

    let layout = Stack {
        gap: Spacing::Md,
        align: FlexAlign::Stretch,
        children: vec![nav_tabs, content_host],
        padding: None,
        justify: None,
        style: None,
        responsive: None,
    }
    .into_component("root")
    .expect("Stack encode");

    let shell = PanelShell {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: epoch,
        layout,
        slots: vec![SlotDecl {
            id: SLOT_ID.into(),
            semantics: SlotSemantics::MainContent,
            default_state: SlotDefault::Loading,
            cache_policy: CachePolicy::None,
            visibility: SlotVisibility::Always,
            max_payload_bytes: None,
        }],
        initial_state: vec![
            StateEntry {
                path: state_path("active_tab"),
                value: CborValue::Text("all".into()),
            },
            StateEntry {
                path: state_path("companies_count"),
                value: CborValue::U64(0),
            },
            StateEntry {
                path: state_path("persons_count"),
                value: CborValue::U64(0),
            },
            StateEntry {
                path: state_path("relations_count"),
                value: CborValue::U64(0),
            },
        ],
        initial_commands: vec![],
    };

    send_ui(&UiPayload::PanelShell(shell));
}

fn send_tab_content(tab: &str) {
    let fragment = build_tab_content(tab);
    let epoch = unsafe { PANEL_EPOCH };

    let slot_content = SlotContent {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: epoch,
        slot_id: SLOT_ID.into(),
        fragment,
        state_overlay: Some(vec![
            StateEntry {
                path: state_path("active_tab"),
                value: CborValue::Text(tab.into()),
            },
        ]),
    };

    send_ui(&UiPayload::SlotContent(slot_content));
    update_stats_state();
}

fn update_stats_state() {
    let companies = count_table("companies").unwrap_or(0) as u64;
    let persons = count_table("persons").unwrap_or(0) as u64;
    let relations = count_table("person_relations").unwrap_or(0) as u64;

    let base = unsafe { STATE_REVISION };
    unsafe { STATE_REVISION += 1 };
    let new_rev = unsafe { STATE_REVISION };

    let patch = StatePatch {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: unsafe { PANEL_EPOCH },
        base_revision: base,
        new_revision: new_rev,
        ops: vec![
            PatchOp { path: state_path("companies_count"), op: PatchOpKind::Set { value: CborValue::U64(companies) } },
            PatchOp { path: state_path("persons_count"), op: PatchOpKind::Set { value: CborValue::U64(persons) } },
            PatchOp { path: state_path("relations_count"), op: PatchOpKind::Set { value: CborValue::U64(relations) } },
        ],
    };

    send_ui(&UiPayload::StatePatch(patch));
}

fn build_tab_content(tab: &str) -> Component {
    match tab {
        "companies" | "company-detail" => build_companies_tab(),
        "persons" | "person-detail" => build_persons_tab(),
        "relationship-map" => build_relationship_map_tab(),
        "smart-lists" => build_smart_lists_tab(),
        _ => build_all_tab(),
    }
}

// =============================================================================
// Tab builders — return Component trees
// =============================================================================

fn build_all_tab() -> Component {
    let companies = count_table("companies").unwrap_or(0);
    let persons = count_table("persons").unwrap_or(0);
    let relations = count_table("person_relations").unwrap_or(0);

    let heading = heading_component("heading-all", "Kontakty");
    let stats = build_stat_row(companies, persons, relations);
    let table = build_contacts_table(None, 100);

    stack_component("tab-all", &[heading, stats, table])
}

fn build_companies_tab() -> Component {
    let heading = heading_component("heading-companies", "Firmy");
    let form = build_company_form();
    let table = build_contacts_table(Some("company"), 100);

    stack_component("tab-companies", &[heading, form, table])
}

fn build_persons_tab() -> Component {
    let heading = heading_component("heading-persons", "Osoby");
    let form = build_person_form();
    let table = build_persons_table_component();

    stack_component("tab-persons", &[heading, form, table])
}

fn build_relationship_map_tab() -> Component {
    let company_id = match first_company_id() {
        Ok(Some(id)) => id,
        _ => return empty_state_component("rel-empty", "Brak mapy relacji", "Mapa relacji powstanie po dodaniu firmy i osob przypietych do niej."),
    };

    let graph = match relationship_map_for_company(&company_id) {
        Ok(g) => g,
        Err(_) => return empty_state_component("rel-err", "Blad", "Nie mozna wczytac mapy relacji."),
    };

    let nodes = graph.get("nodes").and_then(JsonValue::as_array).cloned().unwrap_or_default();
    if nodes.is_empty() {
        return empty_state_component("rel-empty-nodes", "Brak osob w mapie", "Przypnij osoby do firmy przez attach_person_to_company.");
    }

    text_component("rel-map-info", &format!("Mapa relacji: {} osob w grafie", nodes.len()))
}

fn build_smart_lists_tab() -> Component {
    let rows = sql_query_raw(
        "SELECT id, name, kind, is_public FROM smart_lists ORDER BY updated_at DESC LIMIT 24",
        "[]",
    );

    match rows {
        Ok(r) if !r.is_empty() => {
            let info = format!("{} smart lists", r.len());
            text_component("smart-lists-info", &info)
        }
        _ => empty_state_component("smart-empty", "Brak smart lists", "Zapisane filtry pojawia sie tutaj po utworzeniu listy."),
    }
}

// =============================================================================
// Component builders
// =============================================================================

fn text_component(id: &str, content: &str) -> Component {
    Text {
        content: lit(content),
        style: TextStyle::Body,
        tone: None,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
        streaming: None,
    }
    .into_component(id)
    .expect("Text encode")
}

fn heading_component(id: &str, content: &str) -> Component {
    Heading {
        content: lit(content),
        level: 2,
        tone: None,
        align: None,
    }
    .into_component(id)
    .expect("Heading encode")
}

fn stack_component(id: &str, children: &[Component]) -> Component {
    Stack {
        gap: Spacing::Md,
        align: FlexAlign::Stretch,
        children: children.to_vec(),
        padding: Some(Spacing::Md),
        justify: None,
        style: None,
        responsive: None,
    }
    .into_component(id)
    .expect("Stack encode")
}

fn empty_state_component(id: &str, title: &str, message: &str) -> Component {
    Text {
        content: lit(&format!("{} — {}", title, message)),
        style: TextStyle::Body,
        tone: Some(Tone::Muted),
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
        streaming: None,
    }
    .into_component(id)
    .expect("Text encode")
}

fn build_stat_row(companies: i64, persons: i64, relations: i64) -> Component {
    let text = format!(
        "Firmy: {} | Osoby: {} | Relacje: {}",
        companies, persons, relations
    );
    text_component("stats-row", &text)
}

fn build_contacts_table(kind: Option<&str>, limit: i64) -> Component {
    let mut rows_text = Vec::new();

    if kind != Some("person") {
        if let Ok(companies) = search_companies("", limit) {
            for c in &companies {
                let name = json_str(c, "display_name")
                    .or_else(|| json_str(c, "name"))
                    .unwrap_or_else(|| "Firma".into());
                let nip = json_str(c, "nip").unwrap_or_default();
                rows_text.push(format!("[Firma] {} (NIP: {})", name, nip));
            }
        }
    }

    if kind != Some("company") {
        if let Ok(persons) = search_persons("", limit) {
            for p in &persons {
                let name = json_str(p, "full_name").unwrap_or_else(|| "Osoba".into());
                let email = json_str(p, "email_primary").unwrap_or_default();
                rows_text.push(format!("[Osoba] {} ({})", name, email));
            }
        }
    }

    rows_text.truncate(limit as usize);

    if rows_text.is_empty() {
        return empty_state_component("table-empty", "Brak kontaktow", "Dodaj firme albo osobe przez narzedzia Contacts.");
    }

    let content = rows_text.join("\n");
    text_component("contacts-table", &content)
}

fn build_persons_table_component() -> Component {
    let persons = search_persons("", 100).unwrap_or_default();
    if persons.is_empty() {
        return empty_state_component("persons-table-empty", "Brak osob", "Brak osob w bazie Contacts.");
    }

    let mut lines = Vec::new();
    for p in &persons {
        let name = json_str(p, "full_name").unwrap_or_else(|| "Osoba".into());
        let role = json_str(p, "position_title").unwrap_or_default();
        let company = json_str(p, "company_display_name").unwrap_or_default();
        lines.push(format!("{} | {} | {}", name, role, company));
    }

    text_component("persons-table", &lines.join("\n"))
}

fn build_company_form() -> Component {
    text_component("company-form-hint", "Nowa firma: uzyj narzedzia create_company lub akcji UI create-company-ui")
}

fn build_person_form() -> Component {
    text_component("person-form-hint", "Nowa osoba: uzyj narzedzia create_person lub akcji UI create-person-ui")
}

// =============================================================================
// CBOR helpers
// =============================================================================

fn nav_tab(id: &str, label: &str) -> NavTab {
    NavTab {
        id: id.into(),
        label: lit(label),
        icon: None,
        badge: None,
        panel_id: None,
        locked: false,
    }
}

fn lit(text: &str) -> BindRef {
    BindRef::Literal(CborValue::Text(text.into()))
}

fn bound(key: &str) -> BindRef {
    BindRef::Bound(state_path(key))
}

fn state_path(key: &str) -> StatePath {
    StatePath::new(vec![PathSegment::Key(key.into())])
}

// =============================================================================
// Tool registration
// =============================================================================

fn register_contacts_tools() {
    register_tool_json("search_contacts", "Wyszukuje firmy i osoby po nazwie, NIP, REGON, emailu albo stanowisku.", r#"{"type":"object","properties":{"query":{"type":"string"},"kind":{"type":"string","enum":["company","person"]},"limit":{"type":"number","minimum":1,"maximum":100}}}"#);
    register_tool_json("create_company", "Tworzy firme recznie albo po aktualnym lookup online z Wykazu VAT MF.", r#"{"type":"object","properties":{"name":{"type":"string"},"nip":{"type":"string"},"regon":{"type":"string"},"online_lookup":{"type":"boolean"}}}"#);
    register_tool_json("create_person", "Tworzy osobe i opcjonalnie przypina ja do firmy.", r#"{"type":"object","properties":{"full_name":{"type":"string"},"first_name":{"type":"string"},"last_name":{"type":"string"},"email":{"type":"string"},"company_id":{"type":"string"},"position_title":{"type":"string"}}}"#);
    register_tool_json("lookup_company_online", "Pobiera aktualne dane firmy z oficjalnego Wykazu VAT MF bez cache.", r#"{"type":"object","properties":{"nip":{"type":"string"},"regon":{"type":"string"}}}"#);
    register_tool_json("get_company", "Pobiera szczegoly firmy po ID.", r#"{"type":"object","properties":{"company_id":{"type":"string"}},"required":["company_id"]}"#);
    register_tool_json("get_person", "Pobiera szczegoly osoby po ID.", r#"{"type":"object","properties":{"person_id":{"type":"string"}},"required":["person_id"]}"#);
    register_tool_json("attach_person_to_company", "Przypina osobe do firmy z opcjonalnym stanowiskiem.", r#"{"type":"object","properties":{"person_id":{"type":"string"},"company_id":{"type":"string"},"position_title":{"type":"string"}},"required":["person_id","company_id"]}"#);
    register_tool_json("list_persons_in_company", "Listuje osoby przypiety do firmy.", r#"{"type":"object","properties":{"company_id":{"type":"string"},"current_only":{"type":"boolean"}},"required":["company_id"]}"#);
    register_tool_json("get_relationship_map", "Pobiera graf relacji firmy albo osoby.", r#"{"type":"object","properties":{"company_id":{"type":"string"},"person_id":{"type":"string"}}}"#);
    register_tool_json("extract_from_text", "Wyciaga firmy, osoby i relacje z tekstu przez LLM.", r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#);
    register_tool_json("compute_person_insights", "Generuje wnioski handlowe dla osoby na podstawie relacji.", r#"{"type":"object","properties":{"person_id":{"type":"string"}},"required":["person_id"]}"#);
}

fn register_tool_json(name: &str, description: &str, schema: &str) {
    let def = serde_json::json!({
        "name": name,
        "description": description,
        "parameters": serde_json::from_str::<JsonValue>(schema).unwrap_or(serde_json::json!({}))
    });
    let def_str = serde_json::to_string(&def).unwrap_or_default();
    unsafe {
        tool_register(def_str.as_ptr() as i32, def_str.len() as i32);
    }
}

// =============================================================================
// Tool dispatch
// =============================================================================

fn handle_tool(tool_name: &str, params: &JsonValue) -> JsonValue {
    if tool_name.starts_with("ui.") {
        return handle_ui_action(tool_name, params);
    }
    match tool_name {
        "search_contacts" => handle_search_contacts(params),
        "get_company" => handle_get_company(params),
        "get_person" => handle_get_person(params),
        "create_company" => handle_create_company(params),
        "create_person" => handle_create_person(params),
        "attach_person_to_company" => handle_attach_person_to_company(params),
        "list_persons_in_company" => handle_list_persons_in_company(params),
        "get_relationship_map" => handle_get_relationship_map(params),
        "lookup_company_online" => handle_lookup_company_online(params),
        "extract_from_text" => handle_extract_from_text(params),
        "compute_person_insights" => handle_compute_person_insights(params),
        _ => json_error(format!("Nieznane narzedzie: {}", tool_name)),
    }
}

fn handle_ui_action(tool_name: &str, params: &JsonValue) -> JsonValue {
    let action_id = tool_name.rsplit('.').next().unwrap_or("");
    match action_id {
        "panel-navigate" => {
            let tab = optional_string(params, "panel_id")
                .or_else(|| optional_string(params, "id"))
                .unwrap_or_else(|| "all".to_string());
            send_tab_content(&tab);
            serde_json::json!({"ok": true, "panel_id": tab})
        }
        "refresh" => {
            send_tab_content("all");
            serde_json::json!({"ok": true})
        }
        "create-company-ui" => match save_company(params) {
            Ok(company) => {
                notify("Firma zapisana", "", "success");
                send_tab_content("companies");
                serde_json::json!({"ok": true, "company": company})
            }
            Err(e) => json_error(e),
        },
        "create-person-ui" => match save_person(params) {
            Ok(person) => {
                notify("Osoba zapisana", "", "success");
                send_tab_content("persons");
                serde_json::json!({"ok": true, "person": person})
            }
            Err(e) => json_error(e),
        },
        _ => serde_json::json!({"ok": true, "ignored": action_id}),
    }
}

fn handle_flow_block(block_type: &str, params: &JsonValue) -> JsonValue {
    match block_type {
        "contacts.search_contacts" => handle_search_contacts(params),
        "contacts.find_or_create_company" => handle_find_or_create_company(params),
        "contacts.find_or_create_person" => handle_find_or_create_person(params),
        "contacts.lookup_company_online" => handle_lookup_company_online(params),
        _ => json_error(format!("Nieznany flow block: {}", block_type)),
    }
}

// =============================================================================
// Tool handlers
// =============================================================================

fn handle_search_contacts(params: &JsonValue) -> JsonValue {
    let query = optional_string(params, "query").unwrap_or_default();
    let kind = optional_string(params, "kind");
    let limit = read_limit(params);

    let mut items = Vec::new();
    if kind.as_deref() != Some("person") {
        match search_companies(&query, limit) {
            Ok(mut rows) => items.append(&mut rows),
            Err(e) => return json_error(e),
        }
    }
    if kind.as_deref() != Some("company") {
        match search_persons(&query, limit) {
            Ok(mut rows) => items.append(&mut rows),
            Err(e) => return json_error(e),
        }
    }

    items.truncate(limit as usize);
    serde_json::json!({"ok": true, "items": items, "count": items.len()})
}

fn handle_get_company(params: &JsonValue) -> JsonValue {
    let company_id = match required_string(params, "company_id") {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    let company = match get_company_by_id(&company_id) {
        Ok(Some(v)) => v,
        Ok(None) => return json_error("Firma nie istnieje"),
        Err(e) => return json_error(e),
    };
    let persons = match list_persons_for_company(&company_id, false) {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    let group = match company_group(&company_id) {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    serde_json::json!({"ok": true, "company": company, "persons": persons, "group": group})
}

fn handle_get_person(params: &JsonValue) -> JsonValue {
    let person_id = match required_string(params, "person_id") {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    let person = match get_person_by_id(&person_id) {
        Ok(Some(v)) => v,
        Ok(None) => return json_error("Osoba nie istnieje"),
        Err(e) => return json_error(e),
    };
    let relations = match relations_for_person(&person_id) {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    serde_json::json!({"ok": true, "person": person, "relations": relations})
}

fn handle_create_company(params: &JsonValue) -> JsonValue {
    let mut input = params.clone();
    if params.get("online_lookup").and_then(JsonValue::as_bool).unwrap_or(false) {
        match lookup_company_from_params(params) {
            Ok(lookup) => {
                if let Some(company) = lookup.get("company").and_then(JsonValue::as_object) {
                    for (key, value) in company {
                        if !value.is_null() && input.get(key).is_none() {
                            input[key] = value.clone();
                        }
                    }
                }
            }
            Err(e) => return json_error(e),
        }
    }
    match save_company(&input) {
        Ok(company) => serde_json::json!({"ok": true, "company": company}),
        Err(e) => json_error(e),
    }
}

fn handle_find_or_create_company(params: &JsonValue) -> JsonValue {
    match find_company(params) {
        Ok(Some(company)) => serde_json::json!({"ok": true, "company": company, "created": false}),
        Ok(None) => match handle_create_company(params) {
            JsonValue::Object(mut obj) => {
                obj.insert("created".to_string(), JsonValue::Bool(true));
                JsonValue::Object(obj)
            }
            v => v,
        },
        Err(e) => json_error(e),
    }
}

fn handle_create_person(params: &JsonValue) -> JsonValue {
    match save_person(params) {
        Ok(person) => {
            if let Some(company_id) = optional_string(params, "company_id") {
                let attach_params = serde_json::json!({
                    "person_id": person["id"],
                    "company_id": company_id,
                    "position_title": optional_string(params, "position_title")
                });
                if let JsonValue::Object(obj) = handle_attach_person_to_company(&attach_params) {
                    if obj.get("ok").and_then(JsonValue::as_bool) != Some(true) {
                        return JsonValue::Object(obj);
                    }
                }
            }
            serde_json::json!({"ok": true, "person": person})
        }
        Err(e) => json_error(e),
    }
}

fn handle_find_or_create_person(params: &JsonValue) -> JsonValue {
    match find_person(params) {
        Ok(Some(person)) => serde_json::json!({"ok": true, "person": person, "created": false}),
        Ok(None) => match handle_create_person(params) {
            JsonValue::Object(mut obj) => {
                obj.insert("created".to_string(), JsonValue::Bool(true));
                JsonValue::Object(obj)
            }
            v => v,
        },
        Err(e) => json_error(e),
    }
}

fn handle_attach_person_to_company(params: &JsonValue) -> JsonValue {
    let person_id = match required_string(params, "person_id") {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    let company_id = match required_string(params, "company_id") {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    let position_title = optional_string(params, "position_title");
    match attach_person_to_company(&person_id, &company_id, position_title.as_deref()) {
        Ok(employment) => serde_json::json!({"ok": true, "employment": employment}),
        Err(e) => json_error(e),
    }
}

fn handle_list_persons_in_company(params: &JsonValue) -> JsonValue {
    let company_id = match required_string(params, "company_id") {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    let current_only = params.get("current_only").and_then(JsonValue::as_bool).unwrap_or(true);
    match list_persons_for_company(&company_id, current_only) {
        Ok(items) => serde_json::json!({"ok": true, "items": items, "count": items.len()}),
        Err(e) => json_error(e),
    }
}

fn handle_get_relationship_map(params: &JsonValue) -> JsonValue {
    if let Some(company_id) = optional_string(params, "company_id") {
        return match relationship_map_for_company(&company_id) {
            Ok(graph) => serde_json::json!({"ok": true, "graph": graph}),
            Err(e) => json_error(e),
        };
    }
    if let Some(person_id) = optional_string(params, "person_id") {
        return match relationship_map_for_person(&person_id) {
            Ok(graph) => serde_json::json!({"ok": true, "graph": graph}),
            Err(e) => json_error(e),
        };
    }
    json_error("Wymagany company_id albo person_id")
}

fn handle_lookup_company_online(params: &JsonValue) -> JsonValue {
    match lookup_company_from_params(params) {
        Ok(v) => v,
        Err(e) => json_error(e),
    }
}

fn handle_extract_from_text(params: &JsonValue) -> JsonValue {
    let text = match required_string(params, "text") {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    let prompt = format!(
        "Wyciagnij z tekstu firmy, osoby, stanowiska, relacje raportowania i role sprzedazowe. Zwroc wylacznie JSON z polami companies, persons, employments, relations, sales_roles. Nie zapisuj danych.\n\nTEKST:\n{}",
        text
    );
    match generate(&prompt) {
        Ok(answer) => serde_json::json!({"ok": true, "mode": "draft", "raw": answer}),
        Err(e) => json_error(format!("Blad LLM: {}", e)),
    }
}

fn handle_compute_person_insights(params: &JsonValue) -> JsonValue {
    let person_id = match required_string(params, "person_id") {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    let person = match get_person_by_id(&person_id) {
        Ok(Some(v)) => v,
        Ok(None) => return json_error("Osoba nie istnieje"),
        Err(e) => return json_error(e),
    };
    let relations = match relations_for_person(&person_id) {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    let prompt = format!(
        "Na podstawie danych Contacts wygeneruj maksymalnie 3 konkretne wnioski dla handlowca. Nie wymyslaj danych CRM, kalendarza ani maili. Zwroc JSON array stringow.\n\nOSOBA:\n{}\n\nRELACJE:\n{}",
        compact_json(&person),
        compact_json(&serde_json::json!(relations))
    );
    match generate(&prompt) {
        Ok(answer) => serde_json::json!({"ok": true, "mode": "suggest", "insights_raw": answer}),
        Err(e) => json_error(format!("Blad LLM: {}", e)),
    }
}

// =============================================================================
// Data persistence — save/find
// =============================================================================

fn save_company(input: &JsonValue) -> Result<JsonValue, String> {
    let nip = optional_digits(input, "nip");
    let regon = optional_digits(input, "regon");
    let name = optional_string(input, "name")
        .or_else(|| optional_string(input, "display_name"))
        .ok_or_else(|| "Wymagana nazwa firmy albo online_lookup po NIP/REGON".to_string())?;
    let dn = display_name(&name);
    let id = company_id(&name, nip.as_deref(), regon.as_deref());
    let now = unix_time();
    let address_street = optional_string(input, "address_street")
        .or_else(|| optional_string(input, "address"));

    let params = sql_params(&[
        SqlParam::Str(&id),
        SqlParam::Str(&name),
        SqlParam::Str(&dn),
        SqlParam::OptStr(nip.as_deref()),
        SqlParam::OptStr(regon.as_deref()),
        SqlParam::OptStr(optional_digits(input, "krs").as_deref()),
        SqlParam::OptStr(optional_string(input, "vat_id").as_deref()),
        SqlParam::OptStr(address_street.as_deref()),
        SqlParam::OptStr(optional_string(input, "address_city").as_deref()),
        SqlParam::OptStr(optional_string(input, "address_postal").as_deref()),
        SqlParam::Str(optional_string(input, "address_country").as_deref().unwrap_or("PL")),
        SqlParam::OptStr(optional_string(input, "website").as_deref()),
        SqlParam::OptStr(optional_string(input, "phone_main").as_deref()),
        SqlParam::OptStr(optional_string(input, "email_main").as_deref()),
        SqlParam::OptStr(optional_string(input, "industry").as_deref()),
        SqlParam::OptI64(optional_i64(input, "size_employees")),
        SqlParam::OptStr(optional_string(input, "parent_company_id").as_deref()),
        SqlParam::OptF64(optional_f64(input, "parent_share_pct")),
        SqlParam::I64(now),
        SqlParam::I64(now),
        SqlParam::Str(optional_string(input, "source").as_deref().unwrap_or("manual")),
    ]);

    sql_exec_raw(
        "INSERT INTO companies (
            id, name, display_name, nip, regon, krs, vat_id, address_street, address_city,
            address_postal, address_country, website, phone_main, email_main, industry,
            size_employees, parent_company_id, parent_share_pct, created_at, updated_at, source
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            display_name = excluded.display_name,
            nip = COALESCE(excluded.nip, companies.nip),
            regon = COALESCE(excluded.regon, companies.regon),
            krs = COALESCE(excluded.krs, companies.krs),
            vat_id = COALESCE(excluded.vat_id, companies.vat_id),
            address_street = COALESCE(excluded.address_street, companies.address_street),
            address_city = COALESCE(excluded.address_city, companies.address_city),
            address_postal = COALESCE(excluded.address_postal, companies.address_postal),
            address_country = excluded.address_country,
            website = COALESCE(excluded.website, companies.website),
            phone_main = COALESCE(excluded.phone_main, companies.phone_main),
            email_main = COALESCE(excluded.email_main, companies.email_main),
            industry = COALESCE(excluded.industry, companies.industry),
            size_employees = COALESCE(excluded.size_employees, companies.size_employees),
            parent_company_id = COALESCE(excluded.parent_company_id, companies.parent_company_id),
            parent_share_pct = COALESCE(excluded.parent_share_pct, companies.parent_share_pct),
            updated_at = excluded.updated_at,
            source = excluded.source",
        &params,
    )?;
    get_company_by_id(&id)?.ok_or_else(|| "Nie mozna odczytac zapisanej firmy".to_string())
}

fn save_person(input: &JsonValue) -> Result<JsonValue, String> {
    let (first_name, last_name, full_name) = person_names(input)?;
    let email = optional_string(input, "email")
        .or_else(|| optional_string(input, "email_primary"))
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty());
    let id = person_id(&full_name, email.as_deref());
    let now = unix_time();

    let params = sql_params(&[
        SqlParam::Str(&id),
        SqlParam::Str(&first_name),
        SqlParam::Str(&last_name),
        SqlParam::Str(&full_name),
        SqlParam::OptStr(email.as_deref()),
        SqlParam::OptStr(optional_string(input, "phone_primary").as_deref()),
        SqlParam::OptStr(optional_string(input, "linkedin_url").as_deref()),
        SqlParam::Str(optional_string(input, "kind").as_deref().unwrap_or("external")),
        SqlParam::OptStr(optional_string(input, "user_id").as_deref()),
        SqlParam::OptStr(optional_string(input, "company_id").as_deref()),
        SqlParam::OptStr(optional_string(input, "position_title").as_deref()),
        SqlParam::OptStr(optional_string(input, "language").as_deref()),
        SqlParam::OptI64(optional_i64(input, "rodo_consent_at")),
        SqlParam::OptStr(optional_string(input, "notes").as_deref()),
        SqlParam::I64(now),
        SqlParam::I64(now),
        SqlParam::Str(optional_string(input, "source").as_deref().unwrap_or("manual")),
    ]);

    sql_exec_raw(
        "INSERT INTO persons (
            id, first_name, last_name, full_name, email_primary, phone_primary, linkedin_url,
            kind, user_id, current_employer_company_id, current_position_in_company, language,
            rodo_consent_at, notes, created_at, updated_at, source
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
            first_name = excluded.first_name,
            last_name = excluded.last_name,
            full_name = excluded.full_name,
            email_primary = COALESCE(excluded.email_primary, persons.email_primary),
            phone_primary = COALESCE(excluded.phone_primary, persons.phone_primary),
            linkedin_url = COALESCE(excluded.linkedin_url, persons.linkedin_url),
            kind = excluded.kind,
            user_id = COALESCE(excluded.user_id, persons.user_id),
            current_employer_company_id = COALESCE(excluded.current_employer_company_id, persons.current_employer_company_id),
            current_position_in_company = COALESCE(excluded.current_position_in_company, persons.current_position_in_company),
            language = COALESCE(excluded.language, persons.language),
            rodo_consent_at = COALESCE(excluded.rodo_consent_at, persons.rodo_consent_at),
            notes = COALESCE(excluded.notes, persons.notes),
            updated_at = excluded.updated_at,
            source = excluded.source",
        &params,
    )?;

    if let Some(ref email_value) = email {
        let email_id = format!("email:{}", stable_slug(email_value));
        let ep = sql_params(&[
            SqlParam::Str(&email_id),
            SqlParam::Str(&id),
            SqlParam::Str(email_value),
            SqlParam::I64(1),
            SqlParam::I64(now),
        ]);
        sql_exec_raw(
            "INSERT INTO person_emails (id, person_id, value, is_primary, created_at)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(value) DO UPDATE SET person_id = excluded.person_id, is_primary = 1",
            &ep,
        )?;
    }
    get_person_by_id(&id)?.ok_or_else(|| "Nie mozna odczytac zapisanej osoby".to_string())
}

fn attach_person_to_company(
    person_id: &str,
    company_id: &str,
    position_title: Option<&str>,
) -> Result<JsonValue, String> {
    if get_person_by_id(person_id)?.is_none() {
        return Err("Osoba nie istnieje".to_string());
    }
    if get_company_by_id(company_id)?.is_none() {
        return Err("Firma nie istnieje".to_string());
    }
    let id = format!("employment:{}:{}", stable_slug(person_id), stable_slug(company_id));
    let now = unix_time();

    let params = sql_params(&[
        SqlParam::Str(&id),
        SqlParam::Str(person_id),
        SqlParam::Str(company_id),
        SqlParam::OptStr(position_title),
        SqlParam::I64(now),
        SqlParam::I64(now),
    ]);
    sql_exec_raw(
        "INSERT INTO company_persons
            (id, person_id, company_id, position_title, is_current, is_primary, created_at, updated_at)
        VALUES (?, ?, ?, ?, 1, 1, ?, ?)
        ON CONFLICT(person_id, company_id) WHERE is_current = 1
        DO UPDATE SET position_title = COALESCE(excluded.position_title, company_persons.position_title), updated_at = excluded.updated_at",
        &params,
    )?;

    let update_params = sql_params(&[
        SqlParam::Str(company_id),
        SqlParam::OptStr(position_title),
        SqlParam::I64(now),
        SqlParam::Str(person_id),
    ]);
    sql_exec_raw(
        "UPDATE persons
        SET current_employer_company_id = ?, current_position_in_company = COALESCE(?, current_position_in_company), updated_at = ?
        WHERE id = ?",
        &update_params,
    )?;

    Ok(serde_json::json!({
        "id": id,
        "person_id": person_id,
        "company_id": company_id,
        "position_title": position_title
    }))
}

fn find_company(params: &JsonValue) -> Result<Option<JsonValue>, String> {
    if let Some(nip) = optional_digits(params, "nip") {
        return query_company_by_field("nip", &nip);
    }
    if let Some(regon) = optional_digits(params, "regon") {
        return query_company_by_field("regon", &regon);
    }
    if let Some(name) = optional_string(params, "name") {
        return query_company_by_field("display_name", &display_name(&name));
    }
    Ok(None)
}

fn find_person(params: &JsonValue) -> Result<Option<JsonValue>, String> {
    if let Some(email) = optional_string(params, "email") {
        let normalized = email.trim().to_lowercase();
        return query_person_by_field("email_primary", &normalized);
    }
    let (_, _, full_name) = person_names(params)?;
    query_person_by_field("full_name", &full_name)
}

// =============================================================================
// Data access — SQL queries
// =============================================================================

fn search_companies(query: &str, limit: i64) -> Result<Vec<JsonValue>, String> {
    let like = format!("%{}%", query.trim());
    let params = sql_params(&[
        SqlParam::Str(&like),
        SqlParam::Str(&like),
        SqlParam::Str(&like),
        SqlParam::Str(&like),
        SqlParam::I64(limit),
    ]);
    let rows = sql_query_raw(
        "SELECT id, name, display_name, nip, regon, address_city, address_street, website, industry
        FROM companies
        WHERE is_active = 1 AND (? = '%%' OR name LIKE ? OR nip LIKE ? OR regon LIKE ?)
        ORDER BY display_name
        LIMIT ?",
        &params,
    )?;
    Ok(rows.iter().map(|r| company_search_row(r)).collect())
}

fn search_persons(query: &str, limit: i64) -> Result<Vec<JsonValue>, String> {
    let like = format!("%{}%", query.trim());
    let params = sql_params(&[
        SqlParam::Str(&like),
        SqlParam::Str(&like),
        SqlParam::Str(&like),
        SqlParam::Str(&like),
        SqlParam::I64(limit),
    ]);
    let rows = sql_query_raw(
        "SELECT p.id, p.full_name, p.email_primary, p.phone_primary, p.current_position_in_company,
            p.current_employer_company_id, c.display_name
        FROM persons p
        LEFT JOIN companies c ON c.id = p.current_employer_company_id
        WHERE p.is_active = 1 AND (? = '%%' OR p.full_name LIKE ? OR p.email_primary LIKE ? OR p.current_position_in_company LIKE ?)
        ORDER BY p.full_name
        LIMIT ?",
        &params,
    )?;
    Ok(rows.iter().map(|r| person_search_row(r)).collect())
}

fn get_company_by_id(id: &str) -> Result<Option<JsonValue>, String> {
    let params = sql_params(&[SqlParam::Str(id)]);
    let row = sql_query_one_raw(
        "SELECT id, name, display_name, nip, regon, krs, vat_id, address_street, address_city,
            address_postal, address_country, website, phone_main, email_main, industry,
            size_employees, parent_company_id, parent_share_pct, is_active, created_at, updated_at, source
        FROM companies WHERE id = ?",
        &params,
    )?;
    Ok(row.as_ref().map(|r| company_full_row(r)))
}

fn get_person_by_id(id: &str) -> Result<Option<JsonValue>, String> {
    let params = sql_params(&[SqlParam::Str(id)]);
    let row = sql_query_one_raw(
        "SELECT p.id, p.first_name, p.last_name, p.full_name, p.email_primary, p.phone_primary,
            p.linkedin_url, p.kind, p.user_id, p.current_employer_company_id, c.display_name,
            p.current_position_in_company, p.language, p.rodo_consent_at, p.notes, p.is_active,
            p.created_at, p.updated_at, p.source
        FROM persons p
        LEFT JOIN companies c ON c.id = p.current_employer_company_id
        WHERE p.id = ?",
        &params,
    )?;
    Ok(row.as_ref().map(|r| person_full_row(r)))
}

fn query_company_by_field(field: &str, value: &str) -> Result<Option<JsonValue>, String> {
    let query = match field {
        "nip" => "SELECT id FROM companies WHERE nip = ? LIMIT 1",
        "regon" => "SELECT id FROM companies WHERE regon = ? LIMIT 1",
        "display_name" => "SELECT id FROM companies WHERE display_name = ? LIMIT 1",
        _ => return Err("Niepoprawne pole wyszukiwania firmy".to_string()),
    };
    let params = sql_params(&[SqlParam::Str(value)]);
    let row = sql_query_one_raw(query, &params)?;
    match row.and_then(|r| row_str(&r, 0)) {
        Some(id) => get_company_by_id(&id),
        None => Ok(None),
    }
}

fn query_person_by_field(field: &str, value: &str) -> Result<Option<JsonValue>, String> {
    let query = match field {
        "email_primary" => "SELECT id FROM persons WHERE email_primary = ? LIMIT 1",
        "full_name" => "SELECT id FROM persons WHERE full_name = ? LIMIT 1",
        _ => return Err("Niepoprawne pole wyszukiwania osoby".to_string()),
    };
    let params = sql_params(&[SqlParam::Str(value)]);
    let row = sql_query_one_raw(query, &params)?;
    match row.and_then(|r| row_str(&r, 0)) {
        Some(id) => get_person_by_id(&id),
        None => Ok(None),
    }
}

fn first_company_id() -> Result<Option<String>, String> {
    let row = sql_query_one_raw(
        "SELECT id FROM companies WHERE is_active = 1 ORDER BY updated_at DESC LIMIT 1",
        "[]",
    )?;
    Ok(row.and_then(|r| row_str(&r, 0)))
}

fn list_persons_for_company(company_id: &str, current_only: bool) -> Result<Vec<JsonValue>, String> {
    let params = sql_params(&[SqlParam::Str(company_id)]);
    let query = if current_only {
        "SELECT p.id, p.full_name, p.email_primary, p.phone_primary, cp.position_title, cp.department,
            cp.is_current, p.current_position_in_company
        FROM company_persons cp
        JOIN persons p ON p.id = cp.person_id
        WHERE cp.company_id = ? AND cp.is_current = 1
        ORDER BY p.full_name"
    } else {
        "SELECT p.id, p.full_name, p.email_primary, p.phone_primary, cp.position_title, cp.department,
            cp.is_current, p.current_position_in_company
        FROM company_persons cp
        JOIN persons p ON p.id = cp.person_id
        WHERE cp.company_id = ?
        ORDER BY cp.is_current DESC, p.full_name"
    };
    let rows = sql_query_raw(query, &params)?;
    Ok(rows.iter().map(|r| company_person_row(r)).collect())
}

fn relations_for_person(person_id: &str) -> Result<Vec<JsonValue>, String> {
    let params = sql_params(&[SqlParam::Str(person_id), SqlParam::Str(person_id)]);
    let rows = sql_query_raw(
        "SELECT r.id, r.source_person_id, sp.full_name, r.target_person_id, tp.full_name,
            r.company_id, c.display_name, r.relation_type, r.strength, r.evidence
        FROM person_relations r
        JOIN persons sp ON sp.id = r.source_person_id
        JOIN persons tp ON tp.id = r.target_person_id
        LEFT JOIN companies c ON c.id = r.company_id
        WHERE r.source_person_id = ? OR r.target_person_id = ?
        ORDER BY r.relation_type, sp.full_name, tp.full_name",
        &params,
    )?;
    Ok(rows.iter().map(|r| relation_row(r)).collect())
}

fn company_group(company_id: &str) -> Result<JsonValue, String> {
    let params = sql_params(&[SqlParam::Str(company_id), SqlParam::Str(company_id)]);
    let rows = sql_query_raw(
        "SELECT id, display_name, parent_company_id, parent_share_pct
        FROM companies
        WHERE id = ? OR parent_company_id = ?
        ORDER BY parent_company_id IS NOT NULL, display_name",
        &params,
    )?;
    Ok(serde_json::json!({
        "nodes": rows.iter().map(|r| company_group_row(r)).collect::<Vec<_>>()
    }))
}

fn relationship_map_for_company(company_id: &str) -> Result<JsonValue, String> {
    let persons = list_persons_for_company(company_id, true)?;
    let params = sql_params(&[SqlParam::Str(company_id)]);
    let rel_rows = sql_query_raw(
        "SELECT r.id, r.source_person_id, sp.full_name, r.target_person_id, tp.full_name,
            r.company_id, c.display_name, r.relation_type, r.strength, r.evidence
        FROM person_relations r
        JOIN persons sp ON sp.id = r.source_person_id
        JOIN persons tp ON tp.id = r.target_person_id
        LEFT JOIN companies c ON c.id = r.company_id
        WHERE r.company_id = ?
        ORDER BY r.relation_type",
        &params,
    )?;
    let edges: Vec<JsonValue> = rel_rows.iter().map(|r| relation_row(r)).collect();
    Ok(serde_json::json!({
        "scope": "company",
        "company_id": company_id,
        "nodes": persons,
        "edges": edges
    }))
}

fn relationship_map_for_person(person_id: &str) -> Result<JsonValue, String> {
    let person = get_person_by_id(person_id)?.ok_or_else(|| "Osoba nie istnieje".to_string())?;
    let relations = relations_for_person(person_id)?;
    Ok(serde_json::json!({
        "scope": "person",
        "person": person,
        "edges": relations
    }))
}

fn count_table(table: &str) -> Result<i64, String> {
    let query = match table {
        "companies" => "SELECT COUNT(*) FROM companies",
        "persons" => "SELECT COUNT(*) FROM persons",
        "person_relations" => "SELECT COUNT(*) FROM person_relations",
        _ => return Err("Niepoprawna tabela statystyk".to_string()),
    };
    let row = sql_query_one_raw(query, "[]")?;
    Ok(row.and_then(|r| row_i64(&r, 0)).unwrap_or(0))
}

// =============================================================================
// HTTP lookup — MF VAT API
// =============================================================================

fn lookup_company_from_params(params: &JsonValue) -> Result<JsonValue, String> {
    let date = read_date(params);
    if let Some(nip) = optional_digits(params, "nip") {
        if nip.len() != 10 {
            return Err("NIP musi miec 10 cyfr".to_string());
        }
        return lookup_single("nip", &nip, &date);
    }
    if let Some(regon) = optional_digits(params, "regon") {
        if regon.len() != 9 && regon.len() != 14 {
            return Err("REGON musi miec 9 albo 14 cyfr".to_string());
        }
        return lookup_single("regon", &regon, &date);
    }
    Err("Wymagany nip albo regon".to_string())
}

fn lookup_single(kind: &str, identifier: &str, date: &str) -> Result<JsonValue, String> {
    let url = format!("{}/{}/{}?date={}", BASE_API, kind, identifier, date);
    let raw_body = http_get_request(&url)?;
    let raw: JsonValue = serde_json::from_str(&raw_body)
        .map_err(|e| format!("Niepoprawna odpowiedz JSON MF: {}", e))?;
    let result = raw.get("result").cloned().unwrap_or_else(|| serde_json::json!({}));
    let subject = result.get("subject").cloned().unwrap_or(JsonValue::Null);
    if subject.is_null() {
        return Ok(serde_json::json!({
            "ok": true,
            "online": true,
            "cached": false,
            "found": false,
            "date": date,
            "request_id": string_at(&result, "requestId"),
            "raw": raw
        }));
    }
    Ok(serde_json::json!({
        "ok": true,
        "online": true,
        "cached": false,
        "found": true,
        "date": date,
        "request_id": string_at(&result, "requestId"),
        "company": normalize_mf_subject(&subject),
        "raw": raw
    }))
}

fn normalize_mf_subject(subject: &JsonValue) -> JsonValue {
    let address = string_at(subject, "workingAddress")
        .or_else(|| string_at(subject, "residenceAddress"));
    serde_json::json!({
        "name": string_at(subject, "name"),
        "display_name": string_at(subject, "name").map(|v| display_name(&v)),
        "nip": string_at(subject, "nip").map(|v| digits_only(&v)),
        "regon": string_at(subject, "regon").map(|v| digits_only(&v)),
        "krs": string_at(subject, "krs").map(|v| digits_only(&v)),
        "address_street": address,
        "address_country": "PL",
        "source": "mf_vat_whitelist"
    })
}

// =============================================================================
// Row mappers
// =============================================================================

fn company_search_row(row: &Vec<JsonValue>) -> JsonValue {
    serde_json::json!({
        "id": row.get(0),
        "kind": "company",
        "name": row.get(1),
        "display_name": row.get(2),
        "nip": row.get(3),
        "regon": row.get(4),
        "address_city": row.get(5),
        "address_street": row.get(6),
        "website": row.get(7),
        "industry": row.get(8)
    })
}

fn person_search_row(row: &Vec<JsonValue>) -> JsonValue {
    serde_json::json!({
        "id": row.get(0),
        "kind": "person",
        "full_name": row.get(1),
        "email_primary": row.get(2),
        "phone_primary": row.get(3),
        "position_title": row.get(4),
        "company_id": row.get(5),
        "company_display_name": row.get(6)
    })
}

fn company_full_row(row: &Vec<JsonValue>) -> JsonValue {
    serde_json::json!({
        "id": row.get(0),
        "kind": "company",
        "name": row.get(1),
        "display_name": row.get(2),
        "nip": row.get(3),
        "regon": row.get(4),
        "krs": row.get(5),
        "vat_id": row.get(6),
        "address_street": row.get(7),
        "address_city": row.get(8),
        "address_postal": row.get(9),
        "address_country": row.get(10),
        "website": row.get(11),
        "phone_main": row.get(12),
        "email_main": row.get(13),
        "industry": row.get(14),
        "size_employees": row.get(15),
        "parent_company_id": row.get(16),
        "parent_share_pct": row.get(17),
        "is_active": row.get(18),
        "created_at": row.get(19),
        "updated_at": row.get(20),
        "source": row.get(21)
    })
}

fn person_full_row(row: &Vec<JsonValue>) -> JsonValue {
    serde_json::json!({
        "id": row.get(0),
        "kind": "person",
        "first_name": row.get(1),
        "last_name": row.get(2),
        "full_name": row.get(3),
        "email_primary": row.get(4),
        "phone_primary": row.get(5),
        "linkedin_url": row.get(6),
        "person_kind": row.get(7),
        "user_id": row.get(8),
        "current_employer_company_id": row.get(9),
        "current_employer_display_name": row.get(10),
        "current_position_in_company": row.get(11),
        "language": row.get(12),
        "rodo_consent_at": row.get(13),
        "notes": row.get(14),
        "is_active": row.get(15),
        "created_at": row.get(16),
        "updated_at": row.get(17),
        "source": row.get(18)
    })
}

fn company_person_row(row: &Vec<JsonValue>) -> JsonValue {
    serde_json::json!({
        "id": row.get(0),
        "kind": "person",
        "full_name": row.get(1),
        "email_primary": row.get(2),
        "phone_primary": row.get(3),
        "position_title": row.get(4).or(row.get(7)),
        "department": row.get(5),
        "is_current": row.get(6)
    })
}

fn relation_row(row: &Vec<JsonValue>) -> JsonValue {
    serde_json::json!({
        "id": row.get(0),
        "source_person_id": row.get(1),
        "source_person_name": row.get(2),
        "target_person_id": row.get(3),
        "target_person_name": row.get(4),
        "company_id": row.get(5),
        "company_display_name": row.get(6),
        "relation_type": row.get(7),
        "strength": row.get(8),
        "evidence": row.get(9)
    })
}

fn company_group_row(row: &Vec<JsonValue>) -> JsonValue {
    serde_json::json!({
        "id": row.get(0),
        "display_name": row.get(1),
        "parent_company_id": row.get(2),
        "parent_share_pct": row.get(3)
    })
}

// =============================================================================
// Host function wrappers
// =============================================================================

fn log(msg: &str) {
    unsafe { log_info(msg.as_ptr() as i32, msg.len() as i32); }
}

fn log_warning(msg: &str) {
    unsafe { log_warn(msg.as_ptr() as i32, msg.len() as i32); }
}

fn notify(title: &str, body: &str, level: &str) {
    unsafe {
        ui_notify(
            title.as_ptr() as i32, title.len() as i32,
            body.as_ptr() as i32, body.len() as i32,
            level.as_ptr() as i32, level.len() as i32,
        );
    }
}

fn http_get_request(url: &str) -> Result<String, String> {
    let req = serde_json::json!({
        "method": "GET",
        "url": url,
        "headers": {},
        "body": ""
    });
    let req_str = serde_json::to_string(&req).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; HTTP_BUF_SIZE];
    let mut out_len: i32 = 0;

    let rc = unsafe {
        http_request(
            req_str.as_ptr() as i32, req_str.len() as i32,
            buf.as_mut_ptr() as i32, buf.len() as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if rc != 0 {
        return Err(format!("HTTP request failed, code={}", rc));
    }
    if out_len <= 0 || out_len as usize > buf.len() {
        return Err("HTTP response empty or too large".to_string());
    }

    let response_str = core::str::from_utf8(&buf[..out_len as usize])
        .map_err(|_| "HTTP response not valid UTF-8".to_string())?;

    let response: JsonValue = serde_json::from_str(response_str)
        .map_err(|e| format!("HTTP response parse error: {}", e))?;

    let status = response.get("status").and_then(JsonValue::as_i64).unwrap_or(0);
    if status < 200 || status >= 300 {
        return Err(format!("HTTP status {}", status));
    }

    response.get("body").and_then(JsonValue::as_str)
        .map(|s| s.to_string())
        .ok_or_else(|| "HTTP response body missing".to_string())
}

fn generate(prompt: &str) -> Result<String, String> {
    let model = "";
    let options = "";
    let mut buf = vec![0u8; LLM_BUF_SIZE];
    let mut out_len: i32 = 0;

    let rc = unsafe {
        llm_generate(
            prompt.as_ptr() as i32, prompt.len() as i32,
            model.as_ptr() as i32, model.len() as i32,
            options.as_ptr() as i32, options.len() as i32,
            buf.as_mut_ptr() as i32, buf.len() as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if rc != 0 {
        return Err(format!("LLM generate failed, code={}", rc));
    }
    if out_len <= 0 || out_len as usize > buf.len() {
        return Err("LLM response empty or too large".to_string());
    }

    core::str::from_utf8(&buf[..out_len as usize])
        .map(|s| s.to_string())
        .map_err(|_| "LLM response not valid UTF-8".to_string())
}

// =============================================================================
// SQL host function wrappers
// =============================================================================

#[derive(Clone)]
enum SqlParam<'a> {
    Str(&'a str),
    OptStr(Option<&'a str>),
    I64(i64),
    OptI64(Option<i64>),
    OptF64(Option<f64>),
}

fn sql_params(items: &[SqlParam]) -> String {
    let values: Vec<JsonValue> = items.iter().map(|p| match p {
        SqlParam::Str(s) => JsonValue::String(s.to_string()),
        SqlParam::OptStr(Some(s)) => JsonValue::String(s.to_string()),
        SqlParam::OptStr(None) => JsonValue::Null,
        SqlParam::I64(v) => serde_json::json!(*v),
        SqlParam::OptI64(Some(v)) => serde_json::json!(*v),
        SqlParam::OptI64(None) => JsonValue::Null,
        SqlParam::OptF64(Some(v)) => serde_json::json!(*v),
        SqlParam::OptF64(None) => JsonValue::Null,
    }).collect();
    serde_json::to_string(&values).unwrap_or_else(|_| "[]".to_string())
}

fn sql_exec_raw(query: &str, params_json: &str) -> Result<(), String> {
    let mut buf = vec![0u8; 256];
    let mut out_len: i32 = 0;

    let rc = unsafe {
        sql_exec_v1(
            query.as_ptr() as i32, query.len() as i32,
            params_json.as_ptr() as i32, params_json.len() as i32,
            buf.as_mut_ptr() as i32, buf.len() as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if rc != 0 {
        return Err(format!("SQL exec error, code={}", rc));
    }
    Ok(())
}

fn sql_query_raw(query: &str, params_json: &str) -> Result<Vec<Vec<JsonValue>>, String> {
    let mut buf = vec![0u8; SQL_BUF_SIZE];
    let mut out_len: i32 = 0;

    let rc = unsafe {
        sql_query_v1(
            query.as_ptr() as i32, query.len() as i32,
            params_json.as_ptr() as i32, params_json.len() as i32,
            buf.as_mut_ptr() as i32, buf.len() as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if rc != 0 {
        return Err(format!("SQL query error, code={}", rc));
    }

    if out_len <= 0 {
        return Ok(Vec::new());
    }

    let response_str = core::str::from_utf8(&buf[..out_len as usize])
        .map_err(|_| "SQL response not valid UTF-8".to_string())?;

    let rows: Vec<Vec<JsonValue>> = serde_json::from_str(response_str)
        .map_err(|e| format!("SQL response parse error: {}", e))?;

    Ok(rows)
}

fn sql_query_one_raw(query: &str, params_json: &str) -> Result<Option<Vec<JsonValue>>, String> {
    let mut buf = vec![0u8; SQL_BUF_SIZE];
    let mut out_len: i32 = 0;

    let rc = unsafe {
        sql_query_one_v1(
            query.as_ptr() as i32, query.len() as i32,
            params_json.as_ptr() as i32, params_json.len() as i32,
            buf.as_mut_ptr() as i32, buf.len() as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if rc == 10 {
        return Ok(None);
    }
    if rc != 0 {
        return Err(format!("SQL query_one error, code={}", rc));
    }

    if out_len <= 0 {
        return Ok(None);
    }

    let response_str = core::str::from_utf8(&buf[..out_len as usize])
        .map_err(|_| "SQL response not valid UTF-8".to_string())?;

    let row: Vec<JsonValue> = serde_json::from_str(response_str)
        .map_err(|e| format!("SQL response parse error: {}", e))?;

    Ok(Some(row))
}

fn row_str(row: &Vec<JsonValue>, index: usize) -> Option<String> {
    row.get(index).and_then(JsonValue::as_str).map(|s| s.to_string())
}

fn row_i64(row: &Vec<JsonValue>, index: usize) -> Option<i64> {
    row.get(index).and_then(JsonValue::as_i64)
}

// =============================================================================
// Utility functions
// =============================================================================

fn person_names(input: &JsonValue) -> Result<(String, String, String), String> {
    let full = optional_string(input, "full_name");
    let first = optional_string(input, "first_name");
    let last = optional_string(input, "last_name");
    match (first, last, full) {
        (Some(first_name), Some(last_name), _) => {
            let full_name = format!("{} {}", first_name, last_name).trim().to_string();
            Ok((first_name, last_name, full_name))
        }
        (first_name, last_name, Some(full_name)) => {
            let (parsed_first, parsed_last) = split_full_name(&full_name);
            Ok((
                first_name.unwrap_or(parsed_first),
                last_name.unwrap_or(parsed_last),
                full_name,
            ))
        }
        _ => Err("Wymagane full_name albo first_name i last_name".to_string()),
    }
}

fn split_full_name(full_name: &str) -> (String, String) {
    let mut parts = full_name.split_whitespace();
    let first = parts.next().unwrap_or("").to_string();
    let last = parts.collect::<Vec<_>>().join(" ");
    (first, last)
}

fn company_id(name: &str, nip: Option<&str>, regon: Option<&str>) -> String {
    if let Some(v) = nip {
        return format!("company:nip:{}", v);
    }
    if let Some(v) = regon {
        return format!("company:regon:{}", v);
    }
    format!("company:name:{}", stable_slug(name))
}

fn person_id(full_name: &str, email: Option<&str>) -> String {
    if let Some(v) = email {
        return format!("person:email:{}", stable_slug(v));
    }
    format!("person:name:{}", stable_slug(full_name))
}

fn display_name(name: &str) -> String {
    let trimmed = name.trim();
    let without_suffix = trimmed
        .replace(" SPOLKA Z OGRANICZONA ODPOWIEDZIALNOSCIA", "")
        .replace(" SP. Z O.O.", "")
        .replace(" S.A.", "")
        .replace(" SA", "");
    if without_suffix.trim().is_empty() {
        trimmed.to_string()
    } else {
        without_suffix.trim().to_string()
    }
}

fn stable_slug(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn optional_string(params: &JsonValue, key: &str) -> Option<String> {
    params.get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

fn required_string(params: &JsonValue, key: &str) -> Result<String, String> {
    optional_string(params, key).ok_or_else(|| format!("Parametr {} jest wymagany", key))
}

fn optional_digits(params: &JsonValue, key: &str) -> Option<String> {
    optional_string(params, key)
        .map(|v| digits_only(&v))
        .filter(|v| !v.is_empty())
}

fn optional_i64(params: &JsonValue, key: &str) -> Option<i64> {
    params.get(key).and_then(JsonValue::as_i64)
}

fn optional_f64(params: &JsonValue, key: &str) -> Option<f64> {
    params.get(key).and_then(JsonValue::as_f64)
}

fn read_limit(params: &JsonValue) -> i64 {
    params.get("limit")
        .and_then(JsonValue::as_i64)
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT)
}

fn read_date(params: &JsonValue) -> String {
    optional_string(params, "date").unwrap_or_else(current_utc_date)
}

fn current_utc_date() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as i64)
        .unwrap_or(0);
    civil_from_days(days)
}

fn civil_from_days(days_since_epoch: i64) -> String {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    format!("{:04}-{:02}-{:02}", year, m, d)
}

fn digits_only(value: &str) -> String {
    value.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn unix_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn string_at(value: &JsonValue, key: &str) -> Option<String> {
    value.get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

fn json_str(value: &JsonValue, key: &str) -> Option<String> {
    value.get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

fn compact_json(value: &JsonValue) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

fn json_error(message: impl Into<String>) -> JsonValue {
    serde_json::json!({"ok": false, "error": message.into()})
}

// =============================================================================
// Guest memory I/O
// =============================================================================

fn read_guest_string(ptr: i32, len: i32) -> String {
    if len <= 0 {
        return String::new();
    }
    let slice = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    String::from_utf8_lossy(slice).into_owned()
}

fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, response: &JsonValue) -> i32 {
    let response_str = serde_json::to_string(response).unwrap_or_else(|e| {
        format!(
            "{{\"ok\":false,\"error\":\"Blad serializacji odpowiedzi: {}\"}}",
            e
        )
    });
    let bytes = response_str.as_bytes();
    if bytes.len() > out_cap as usize {
        log_warning("Output buffer too small for Contacts response");
        return 2;
    }
    let dest = unsafe { core::slice::from_raw_parts_mut(out_ptr as *mut u8, out_cap as usize) };
    dest[..bytes.len()].copy_from_slice(bytes);
    let len_dest = unsafe { core::slice::from_raw_parts_mut(out_len_ptr as *mut u8, 4) };
    len_dest.copy_from_slice(&(bytes.len() as i32).to_le_bytes());
    0
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_only_removes_formatting() {
        assert_eq!(digits_only("PL 734-286-71-48"), "7342867148");
    }

    #[test]
    fn stable_slug_is_deterministic() {
        assert_eq!(stable_slug("Jan Kowalski / CFO"), "jan-kowalski-cfo");
    }

    #[test]
    fn split_full_name_keeps_compound_last_name() {
        assert_eq!(
            split_full_name("Anna Maria Kowalska"),
            ("Anna".to_string(), "Maria Kowalska".to_string())
        );
    }

    #[test]
    fn civil_date_for_unix_epoch() {
        assert_eq!(civil_from_days(0), "1970-01-01");
    }

    #[test]
    fn display_name_strips_legal_form() {
        assert_eq!(display_name("ACME SP. Z O.O."), "ACME");
        assert_eq!(display_name("BETA S.A."), "BETA");
    }
}
