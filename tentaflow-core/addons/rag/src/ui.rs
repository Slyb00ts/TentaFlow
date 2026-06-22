// =============================================================================
// Plik: addons/rag/src/ui.rs
// Opis: Pelne GUI addona RAG (MemGraphRAG) przez binarny protokol CBOR (sdk-runtime).
//       Panel `main` z NavTabs (5 zakladek): Kolekcje, Dokumenty, Czat, Graf,
//       Konflikty. Komponenty emitowane DEKLARATYWNIE typami SDK
//       (tentaflow_sdk_spec::protocol::ui::*); zero HTML/JS. Akcje UI (Handler ->
//       action_id) wracaja do hosta jako tool "ui.main.<action>", a host wola
//       crate::ui::handle_ui_action, ktory uderza w read/write tooly z lib.rs i
//       odswieza panel przez SlotContent / StatePatch. Stan panelu (wybrana
//       kolekcja, wiersze tabel, wyniki) trzymany w stanie panelu pod sciezkami
//       StatePath; tabele czytaja wiersze z tych sciezek (rows_path).
// =============================================================================

use serde_json::{json, Value as JsonValue};

use tentaflow_sdk_spec::protocol::control::CborMap;
use tentaflow_sdk_spec::protocol::ui::a11y::EventKind;
use tentaflow_sdk_spec::protocol::ui::actions::Button;
use tentaflow_sdk_spec::protocol::ui::bind::{BindRef, PathSegment, StatePath};
use tentaflow_sdk_spec::protocol::ui::component::{Component, HandlerMap};
use tentaflow_sdk_spec::protocol::ui::data::{Heading, Markdown, Table, Text};
use tentaflow_sdk_spec::protocol::ui::form::{FileInput, Input, Select, Textarea};
use tentaflow_sdk_spec::protocol::ui::handler::{FailurePolicy, Handler};
use tentaflow_sdk_spec::protocol::ui::inline::{
    NavTab, SelectOption, SelectValue, TableColumn, TableColumnWidth,
};
use tentaflow_sdk_spec::protocol::ui::layout::{NavTabs, Stack};
use tentaflow_sdk_spec::protocol::ui::molecules::Inspector;
use tentaflow_sdk_spec::protocol::ui::panel::PanelShell;
use tentaflow_sdk_spec::protocol::ui::patch::{PatchOp, PatchOpKind};
use tentaflow_sdk_spec::protocol::ui::slot::{
    CachePolicy, SlotDecl, SlotDefault, SlotSemantics, SlotVisibility, StateEntry,
};
use tentaflow_sdk_spec::protocol::ui::slot_msg::SlotContent;
use tentaflow_sdk_spec::protocol::ui::state::StatePatch;
use tentaflow_sdk_spec::protocol::ui::tokens::{
    ButtonSize, ButtonVariant, ColumnRender, Density, FlexAlign, InputSize, InputType, LinkTarget,
    MarkdownFeature, NavTabsVariant, Spacing, TableSelectMode, TableVariant, TextAlign, TextStyle,
    Tone,
};
use tentaflow_sdk_spec::protocol::ui::ui_payload::UiPayload;
use tentaflow_sdk_spec::protocol::value::Value as CborValue;

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn ui_render_cbor(cbor_ptr: i32, cbor_len: i32) -> i32;
}

// =============================================================================
// Stale i identyfikatory
// =============================================================================

pub const ADDON_ID: &str = "rag";
pub const PANEL_ID: &str = "main";
const SLOT_ID: &str = "content";
pub const DEFAULT_TAB: &str = "collections";

// Zakladki NavTabs.
const TAB_COLLECTIONS: &str = "collections";
const TAB_DOCUMENTS: &str = "documents";
const TAB_CHAT: &str = "chat";
const TAB_GRAPH: &str = "graph";
const TAB_CONFLICTS: &str = "conflicts";

// Sciezki stanu panelu (StatePath::Key). Tabele czytaja wiersze z *_rows; pola
// formularzy bind-uja sie do *_input; wybrana kolekcja steruje zakladkami
// Dokumenty/Czat.
const SP_ACTIVE_TAB: &str = "active_tab";
const SP_COLLECTION_ROWS: &str = "collection_rows";
const SP_NEW_COLLECTION: &str = "new_collection_name";
const SP_SELECTED_COLLECTION: &str = "selected_collection";
const SP_SELECTED_COLLECTION_NAME: &str = "selected_collection_name";
const SP_DOCUMENT_ROWS: &str = "document_rows";
const SP_INGEST_SUMMARY: &str = "ingest_summary";
const SP_CHAT_QUESTION: &str = "chat_question";
const SP_CHAT_COLLECTION: &str = "chat_collection";
const SP_CHAT_ANSWER: &str = "chat_answer";
const SP_CITATION_ROWS: &str = "citation_rows";
const SP_GRAPH_QUERY: &str = "graph_query";
const SP_GRAPH_CENTER: &str = "graph_center";
const SP_NEIGHBOR_ROWS: &str = "neighbor_rows";
const SP_FACT_ROWS: &str = "fact_rows";
const SP_CONFLICT_STATUS: &str = "conflict_status_filter";
const SP_CONFLICT_ROWS: &str = "conflict_rows";
const SP_CONFLICT_DETAIL: &str = "conflict_detail_text";
const SP_STATUS_MESSAGE: &str = "status_message";

// =============================================================================
// Stan modulu (epoch + rewizja stanu + aktywna zakladka)
// =============================================================================

static mut PANEL_EPOCH: u64 = 1;
static mut STATE_REVISION: u64 = 0;

/// Tozsamosc uzytkownika biezacego wywolania (z request JSON `user_id`, ktore host
/// nadaje per-call w `call_tool`). Instancja WASM jest pulowana per-(addon,user),
/// ale KV hosta jest scope'owane TYLKO po addon_id — wiec bez tego pola wartosci
/// pol formularza przeciekalyby miedzy userami/sesjami tej samej instancji. Trzymane
/// jako String, bo `on_request` ustawia je na poczatku kazdego wywolania UI.
static mut SESSION_USER_ID: Option<String> = None;

fn panel_epoch() -> u64 {
    unsafe { PANEL_EPOCH }
}

/// Ustawia tozsamosc usera dla biezacego wywolania UI. Wolane z `handle_ui_action`
/// na poczatku kazdej akcji (host przekazuje `user_id` w request JSON). Pusty/None
/// => sesja anonimowa (klucz "anon"), zeby nie kolidowac z realnymi userami.
pub fn set_session_user(user_id: Option<&str>) {
    let normalized = user_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // Zapis przez surowy wskaznik (bez tworzenia &mut do static mut — unika UB i
    // lintu static_mut_refs); WASM addon jest jednowatkowy w obrebie wywolania.
    unsafe {
        *std::ptr::addr_of_mut!(SESSION_USER_ID) = normalized;
    }
}

/// Identyfikator sesji do kluczowania KV pol formularza: `{user_id}:{panel_epoch}`.
/// `panel_epoch` izoluje rownolegle panele tego samego usera (np. dwie karty), a
/// `user_id` izoluje roznych userow tej samej instancji. Brak usera => "anon".
fn session_id() -> String {
    // Klon przez surowy wskaznik (bez wspoldzielonej referencji do static mut).
    let user = unsafe { (*std::ptr::addr_of!(SESSION_USER_ID)).clone() }
        .unwrap_or_else(|| "anon".to_string());
    format!("{user}:{}", panel_epoch())
}

/// Klucz KV pola formularza scope'owany na biezaca sesje panelu. Eliminuje
/// cross-session bleed: KV hosta jest per-addon, wiec sam `field` bylby wspoldzielony
/// przez wszystkich userow i wszystkie otwarcia panelu tej instancji.
fn session_key(field: &str) -> String {
    format!("f:{}:{field}", session_id())
}

/// Reset stanu modulu na reopen panelu (host nadaje swiezy epoch, resetuje rewizje do 0).
/// Best-effort GC kluczy pol sesji, ktora ta instancja obslugiwala ostatnio (stary
/// `SESSION_USER_ID` + stary epoch), PRZED adopcja nowego epocha — zeby nie zostawic
/// osieroconych wpisow `f:{user}:{epoch}:{field}`. Backstop dla przypadkow, w ktorych
/// instancja jest reuzyta dla innego usera bez reopen: tier `Ephemeral` (RAM-only,
/// eksmitowany pod per-addon cap), wiec wpisy nie utrwalaja sie na dysku.
pub fn reset_for_open(epoch: u64) {
    gc_session_fields();
    set_panel_epoch(epoch);
}

/// Adoptuje nowy epoch panelu i zeruje licznik rewizji — bez GC (czysta mutacja
/// statykow). Wydzielone z `reset_for_open`, by testy mogly ustawiac epoch bez
/// wywolywania host-fn `state_delete` (niedostepnego poza wasm).
fn set_panel_epoch(epoch: u64) {
    unsafe {
        PANEL_EPOCH = epoch;
        STATE_REVISION = 0;
    }
}

/// Adopcja epocha NIESIONEGO przez akcje UI (host-zwalidowany `params.__panel_epoch`).
/// ZRODLO PRAWDY dla biezacego wywolania: statyk PANEL_EPOCH jest tylko cache, bo
/// instancje WASM sa pulowane/reuzywane (statyk moze byc cudzy/stale). Bez tego
/// `session_key`, `set_kv`, `field_value` kluczowalyby pola pod zlym epoch, a emisja
/// StatePatch/SlotContent szlaby z epoch, ktory host odrzuca jako stale.
///
/// Gdy epoch akcji == biezacy: NO-OP — zachowujemy licznik rewizji tej sesji (host
/// sledzi rewizje per-epoch; reset zlamalby base_revision kolejnych patchy). Gdy
/// epoch akcji != biezacy: instancja zostala reuzyta dla innego panelu/karty — adoptujemy
/// nowy epoch i zerujemy rewizje (host startuje licznik per-epoch od 0). Bez GC: to nie
/// reopen, a samo przeklucze biezacego wywolania na poprawny epoch.
pub fn adopt_action_epoch(epoch: u64) {
    if epoch != panel_epoch() {
        set_panel_epoch(epoch);
    }
}

/// Usuwa z KV wszystkie pola formularza sesji identyfikowanej przez biezacy
/// `session_id` (stary user + stary epoch). Wolane przy reopen panelu.
fn gc_session_fields() {
    for field in KNOWN_FIELDS {
        let _ = crate::state_delete(&session_key(field));
    }
}

/// Lista pol formularza utrzymywanych w KV per-sesja (zrodlo prawdy dla GC,
/// allowlisty `set-field` i hydratacji renderu).
const KNOWN_FIELDS: &[&str] = &[
    SP_NEW_COLLECTION,
    SP_SELECTED_COLLECTION,
    SP_SELECTED_COLLECTION_NAME,
    SP_CHAT_QUESTION,
    SP_CHAT_COLLECTION,
    SP_GRAPH_QUERY,
    SP_CONFLICT_STATUS,
];

// =============================================================================
// Wysylka CBOR
// =============================================================================

fn send_ui(payload: &UiPayload) -> i32 {
    let mut buf = Vec::with_capacity(2048);
    minicbor::encode(payload, &mut buf).expect("kodowanie UiPayload");
    unsafe { ui_render_cbor(buf.as_ptr() as i32, buf.len() as i32) }
}

fn state_path(key: &str) -> StatePath {
    StatePath::new(vec![PathSegment::Key(key.into())])
}

fn lit(text: &str) -> BindRef {
    BindRef::Literal(CborValue::Text(text.into()))
}

fn bound(key: &str) -> BindRef {
    BindRef::Bound(state_path(key))
}

// =============================================================================
// PanelShell — NavTabs (5 zakladek) + host slotu tresci
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

fn backend_handler(event: EventKind, action_id: &str) -> (EventKind, Handler) {
    (
        event,
        Handler::Backend {
            action_id: action_id.into(),
            params: CborMap(vec![]),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )
}

/// Handler `set-field` na zmianie wartosci pola formularza. Klik submita NIE niesie
/// wartosci inputow (eventDispatcher: params = {...handler.params, ...dom_event.detail},
/// a klik ma pusty detail), wiec jedyny dzialajacy wzorzec to controlled-by-backend:
/// kazdy Input/Textarea/Select dostaje on-change -> action `set-field`. Renderer
/// emituje `change` z detail `{value}` (form-text/select-renderer), eventDispatcher
/// dokleja `value` do params, a `field` (klucz pola) niesiemy w handler.params.
/// Backend zapisuje value w instancyjnym KV (Durable) pod kluczem pola; submit czyta
/// zebrane pola ze stanu.
fn set_field_handler(event: EventKind, field_key: &str) -> (EventKind, Handler) {
    (
        event,
        Handler::Backend {
            action_id: "set-field".into(),
            params: CborMap(vec![("field".into(), CborValue::Text(field_key.into()))]),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )
}

pub fn send_panel_shell() {
    let mut nav = NavTabs {
        items: vec![
            nav_tab(TAB_COLLECTIONS, "Kolekcje"),
            nav_tab(TAB_DOCUMENTS, "Dokumenty"),
            nav_tab(TAB_CHAT, "Czat"),
            nav_tab(TAB_GRAPH, "Graf"),
            nav_tab(TAB_CONFLICTS, "Konflikty"),
        ],
        active_id: bound(SP_ACTIVE_TAB),
        variant: NavTabsVariant::Underlined,
        scroll_overflow: true,
    }
    .into_component("nav-tabs")
    .expect("kodowanie NavTabs");
    nav.handlers = Some(HandlerMap(vec![backend_handler(
        EventKind::Select,
        "panel-navigate",
    )]));

    let body = Inspector {
        title: lit("RAG"),
        content_slot: SLOT_ID.into(),
        actions: vec![],
        tabs: None,
        collapsible: false,
    }
    .into_component("content-host")
    .expect("kodowanie Inspector");

    let layout = Stack {
        gap: Spacing::Md,
        align: FlexAlign::Stretch,
        children: vec![nav, body],
        padding: None,
    }
    .into_component("root")
    .expect("kodowanie Stack root");

    let shell = PanelShell {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: panel_epoch(),
        layout,
        slots: vec![SlotDecl {
            id: SLOT_ID.into(),
            semantics: SlotSemantics::MainContent,
            default_state: SlotDefault::Loading,
            cache_policy: CachePolicy::None,
            visibility: SlotVisibility::Always,
            max_payload_bytes: None,
        }],
        initial_state: initial_state_entries(),
        initial_commands: vec![],
    };

    send_ui(&UiPayload::PanelShell(shell));
}

/// Stan poczatkowy panelu — wszystkie sciezki, ktorych dotykaja bind-y/tabele,
/// musza istniec od startu (inaczej tabele renderuja sie puste, a inputy bez wartosci).
fn initial_state_entries() -> Vec<StateEntry> {
    let empty_arr = || CborValue::Array(vec![]);
    let empty_str = || CborValue::Text("".into());
    vec![
        StateEntry { path: state_path(SP_ACTIVE_TAB), value: CborValue::Text(DEFAULT_TAB.into()) },
        StateEntry { path: state_path(SP_COLLECTION_ROWS), value: empty_arr() },
        StateEntry { path: state_path(SP_NEW_COLLECTION), value: empty_str() },
        StateEntry { path: state_path(SP_SELECTED_COLLECTION), value: empty_str() },
        StateEntry { path: state_path(SP_SELECTED_COLLECTION_NAME), value: empty_str() },
        StateEntry { path: state_path(SP_DOCUMENT_ROWS), value: empty_arr() },
        StateEntry { path: state_path(SP_INGEST_SUMMARY), value: empty_str() },
        StateEntry { path: state_path(SP_CHAT_QUESTION), value: empty_str() },
        StateEntry { path: state_path(SP_CHAT_COLLECTION), value: empty_str() },
        StateEntry { path: state_path(SP_CHAT_ANSWER), value: empty_str() },
        StateEntry { path: state_path(SP_CITATION_ROWS), value: empty_arr() },
        StateEntry { path: state_path(SP_GRAPH_QUERY), value: empty_str() },
        StateEntry { path: state_path(SP_GRAPH_CENTER), value: empty_str() },
        StateEntry { path: state_path(SP_NEIGHBOR_ROWS), value: empty_arr() },
        StateEntry { path: state_path(SP_FACT_ROWS), value: empty_arr() },
        StateEntry { path: state_path(SP_CONFLICT_STATUS), value: CborValue::Text("open".into()) },
        StateEntry { path: state_path(SP_CONFLICT_ROWS), value: empty_arr() },
        StateEntry { path: state_path(SP_CONFLICT_DETAIL), value: empty_str() },
        StateEntry { path: state_path(SP_STATUS_MESSAGE), value: empty_str() },
    ]
}

// =============================================================================
// SlotContent — fragment zakladki + overlay danych (wiersze tabel)
// =============================================================================

pub fn send_tab_content(tab: &str) {
    let fragment = build_tab(tab);
    let mut overlay = vec![StateEntry {
        path: state_path(SP_ACTIVE_TAB),
        value: CborValue::Text(tab.into()),
    }];
    overlay.extend(tab_data_overlay(tab));
    overlay.extend(tab_field_overlay(tab));

    let slot_content = SlotContent {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: panel_epoch(),
        slot_id: SLOT_ID.into(),
        fragment,
        state_overlay: Some(overlay),
    };
    send_ui(&UiPayload::SlotContent(slot_content));
}

/// Dane (wiersze tabel) ladowane razem z fragmentem zakladki — tabele czytaja je
/// ze sciezek stanu (rows_path), wiec overlay musi je dostarczyc przy renderze.
fn tab_data_overlay(tab: &str) -> Vec<StateEntry> {
    match tab {
        TAB_COLLECTIONS => vec![StateEntry {
            path: state_path(SP_COLLECTION_ROWS),
            value: load_collection_rows(),
        }],
        TAB_DOCUMENTS => {
            let collection = selected_collection();
            let (rows, summary) = if collection.is_empty() {
                (CborValue::Array(vec![]), CborValue::Text("Wybierz kolekcje w zakladce Kolekcje.".into()))
            } else {
                (load_document_rows(&collection), CborValue::Text(load_ingest_summary(&collection)))
            };
            vec![
                StateEntry { path: state_path(SP_DOCUMENT_ROWS), value: rows },
                StateEntry { path: state_path(SP_INGEST_SUMMARY), value: summary },
            ]
        }
        TAB_CONFLICTS => vec![StateEntry {
            path: state_path(SP_CONFLICT_ROWS),
            value: load_conflict_rows(&conflict_status_filter()),
        }],
        // Czat i Graf startuja puste — wypelnia je akcja uzytkownika.
        _ => vec![],
    }
}

/// HYDRATACJA pol formularza: wstawia AKTUALNA per-sesyjna wartosc pola (z KV
/// `f:{user}:{epoch}:{field}`) do renderowanej sciezki stanu, zeby UI pokazywal to
/// samo co backend. Bez tego po nawigacji input bylby pusty (stan panelu hosta nie
/// zna wartosci zebranej w innej zakladce), a submit czytalby z KV — UI≠backend.
/// Hydratujemy TYLKO pola wprowadzane przez usera w danej zakladce; selecty z
/// wartosciami sterowanymi (kolekcja/status) tez, bo ich biezacy wybor zyje w KV.
fn tab_field_overlay(tab: &str) -> Vec<StateEntry> {
    let hydrate = |field: &str, default: &str| StateEntry {
        path: state_path(field),
        value: CborValue::Text(field_value(field).unwrap_or_else(|| default.to_string())),
    };
    match tab {
        TAB_COLLECTIONS => vec![hydrate(SP_NEW_COLLECTION, "")],
        TAB_DOCUMENTS => {
            // Nazwa wybranej kolekcji (display) jest per-sesja — zhydratuj etykiete
            // w tym samym formacie co `action_open_collection` ("Kolekcja: {name}").
            let label = field_value(SP_SELECTED_COLLECTION_NAME)
                .map(|name| format!("Kolekcja: {name}"))
                .unwrap_or_default();
            vec![StateEntry {
                path: state_path(SP_SELECTED_COLLECTION_NAME),
                value: CborValue::Text(label),
            }]
        }
        TAB_CHAT => vec![
            hydrate(SP_CHAT_QUESTION, ""),
            hydrate(SP_CHAT_COLLECTION, ""),
        ],
        TAB_GRAPH => vec![hydrate(SP_GRAPH_QUERY, "")],
        TAB_CONFLICTS => vec![hydrate(SP_CONFLICT_STATUS, "open")],
        _ => vec![],
    }
}

fn build_tab(tab: &str) -> Component {
    match tab {
        TAB_DOCUMENTS => documents_tab(),
        TAB_CHAT => chat_tab(),
        TAB_GRAPH => graph_tab(),
        TAB_CONFLICTS => conflicts_tab(),
        _ => collections_tab(),
    }
}

// =============================================================================
// Buildery komponentow (typowane SDK)
// =============================================================================

fn heading(id: &str, content: &str) -> Component {
    Heading {
        content: lit(content),
        level: 3,
        tone: None,
        align: None,
    }
    .into_component(id)
    .expect("kodowanie Heading")
}

fn body_text(id: &str, content: BindRef) -> Component {
    Text {
        content,
        style: TextStyle::Body,
        tone: None,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
    }
    .into_component(id)
    .expect("kodowanie Text")
}

fn muted_caption(id: &str, content: BindRef) -> Component {
    Text {
        content,
        style: TextStyle::Caption,
        tone: Some(Tone::Muted),
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
    }
    .into_component(id)
    .expect("kodowanie Text caption")
}

fn stack(id: &str, children: Vec<Component>) -> Component {
    Stack {
        gap: Spacing::Md,
        align: FlexAlign::Stretch,
        children,
        padding: Some(Spacing::Md),
    }
    .into_component(id)
    .expect("kodowanie Stack")
}

fn action_button(id: &str, label: &str, action_id: &str, variant: ButtonVariant, tone: Tone) -> Component {
    let mut button = Button {
        variant,
        tone,
        label: lit(label),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }
    .into_component(id)
    .expect("kodowanie Button");
    button.handlers = Some(HandlerMap(vec![backend_handler(EventKind::Click, action_id)]));
    button
}

fn text_input(id: &str, bind_key: &str, label: &str, placeholder: &str) -> Component {
    let mut input = Input {
        r#type: InputType::Text,
        bind_path: state_path(bind_key),
        placeholder: Some(lit(placeholder)),
        label: Some(lit(label)),
        hint: None,
        leading_icon: None,
        trailing_icon: None,
        prefix: None,
        suffix: None,
        validators: vec![],
        max_length: Some(512),
        min_length: None,
        pattern: None,
        autocomplete: None,
        input_mode: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
    }
    .into_component(id)
    .expect("kodowanie Input");
    // Controlled-by-backend: na `change` (commit/blur) i `submit` (Enter) zapisujemy
    // wartosc do KV; klik submita nie niesie wartosci, wiec to jedyna sciezka.
    input.handlers = Some(HandlerMap(vec![
        set_field_handler(EventKind::Change, bind_key),
        set_field_handler(EventKind::Submit, bind_key),
    ]));
    input
}

/// Kolumna tabeli czytajaca pole `field` z wiersza (field_path rooted at row).
fn col(id: &str, header: &str, field: &str, width: TableColumnWidth) -> TableColumn {
    TableColumn {
        id: id.into(),
        header: lit(header),
        field_path: vec![PathSegment::Key(field.into())],
        width,
        render: ColumnRender::Text,
        format: None,
        align: Some(TextAlign::Start),
        sortable: false,
        hidden_by_default: false,
        sticky_left: false,
    }
}

/// Tabela czytajaca wiersze ze sciezki stanu `rows_key`. row_actions to przyciski
/// per-wiersz (Handler -> action_id); host przekazuje klucz wiersza w params.
fn table(
    id: &str,
    rows_key: &str,
    row_key_field: &str,
    columns: Vec<TableColumn>,
    row_actions: Vec<Component>,
) -> Component {
    Table {
        columns,
        rows_path: state_path(rows_key),
        row_key_field: row_key_field.into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: false,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: true,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions,
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }
    .into_component(id)
    .expect("kodowanie Table")
}

/// Przycisk akcji per-wiersz (Table.row_actions). Host wola "ui.main.<action>" z
/// kluczem wiersza (row_key_field) w params.
fn row_action(id: &str, label: &str, action_id: &str, tone: Tone) -> Component {
    let mut button = Button {
        variant: ButtonVariant::Ghost,
        tone,
        label: lit(label),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Sm,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Compact,
    }
    .into_component(id)
    .expect("kodowanie Button row-action");
    button.handlers = Some(HandlerMap(vec![backend_handler(EventKind::Click, action_id)]));
    button
}

// =============================================================================
// Zakladka 1 — Kolekcje
// =============================================================================

fn collections_tab() -> Component {
    let new_name = text_input("col-new-name", SP_NEW_COLLECTION, "Nowa kolekcja", "Nazwa kolekcji");
    let create = action_button(
        "col-create",
        "Utworz kolekcje",
        "create-collection",
        ButtonVariant::Primary,
        Tone::Neutral,
    );
    let refresh = action_button("col-refresh", "Odswiez", "refresh-collections", ButtonVariant::Secondary, Tone::Neutral);

    let tbl = table(
        "col-table",
        SP_COLLECTION_ROWS,
        "id",
        vec![
            col("c-name", "Nazwa", "name", TableColumnWidth::Fr { value: 3 }),
            col("c-docs", "Dokumenty", "document_count", TableColumnWidth::Fr { value: 1 }),
            col("c-created", "Utworzono", "created", TableColumnWidth::Fr { value: 2 }),
        ],
        vec![
            row_action("col-open", "Otworz", "open-collection", Tone::Primary),
            row_action("col-delete", "Usun", "delete-collection", Tone::Critical),
        ],
    );

    stack(
        "tab-collections",
        vec![
            heading("col-heading", "Kolekcje"),
            muted_caption("col-status", bound(SP_STATUS_MESSAGE)),
            new_name,
            create,
            refresh,
            tbl,
        ],
    )
}

// =============================================================================
// Zakladka 2 — Dokumenty + status ingestu
// =============================================================================

fn documents_tab() -> Component {
    let selected = body_text("doc-selected", bound(SP_SELECTED_COLLECTION_NAME));
    let summary = body_text("doc-ingest-summary", bound(SP_INGEST_SUMMARY));

    // FileInput: upload PDF/obraz/tekst. Po wyborze plikow renderer hosta robi
    // chunked-upload kazdego pliku do document store instancji, a NA KONIEC emituje
    // event `upload_complete` z detail `{doc_ref, filename, mime, name, size}`.
    // Nasluchujemy go handlerem -> action `ingest-uploaded` -> pelny ingest
    // (parse->chunk->embedding) na wybranej kolekcji. `doc_ref` to id bloba czytelny
    // przez document_get (ta sama warstwa co `doc_id_blob` w ingest_document).
    let mut upload = FileInput {
        bind_path: state_path("upload_files"),
        accept: vec![
            "application/pdf".into(),
            "image/png".into(),
            "image/jpeg".into(),
            "image/webp".into(),
            "text/plain".into(),
            "text/markdown".into(),
            "application/json".into(),
        ],
        max_size_bytes: 64 * 1024 * 1024,
        max_files: 10,
        multiple: true,
        drag_and_drop: true,
        capture: None,
        upload_action_id: "ingest-uploaded".into(),
        label: Some(lit("Wgraj dokumenty (PDF / obraz / tekst)")),
        hint: Some(lit("Po wgraniu uruchamiany jest pelny ingest: parse -> chunk -> embedding.")),
    }
    .into_component("doc-upload")
    .expect("kodowanie FileInput");
    // Ingest per plik wyzwalany dopiero po zakonczeniu uploadu (upload_complete).
    upload.handlers = Some(HandlerMap(vec![backend_handler(
        EventKind::UploadComplete,
        "ingest-uploaded",
    )]));

    let refresh = action_button("doc-refresh", "Odswiez", "refresh-documents", ButtonVariant::Secondary, Tone::Neutral);

    let tbl = table(
        "doc-table",
        SP_DOCUMENT_ROWS,
        "id",
        vec![
            col("d-name", "Nazwa", "filename", TableColumnWidth::Fr { value: 3 }),
            col("d-mime", "Typ", "mime", TableColumnWidth::Fr { value: 2 }),
            col("d-chunks", "Chunki", "chunk_count", TableColumnWidth::Fr { value: 1 }),
            col("d-ents", "Encje", "entity_count", TableColumnWidth::Fr { value: 1 }),
            col("d-rels", "Relacje", "relation_count", TableColumnWidth::Fr { value: 1 }),
            col("d-graph", "Graf", "graph_flag", TableColumnWidth::Fr { value: 1 }),
            col("d-status", "Status", "status", TableColumnWidth::Fr { value: 1 }),
        ],
        vec![row_action("doc-delete", "Usun", "delete-document", Tone::Critical)],
    );

    stack(
        "tab-documents",
        vec![
            heading("doc-heading", "Dokumenty"),
            selected,
            muted_caption("doc-status", bound(SP_STATUS_MESSAGE)),
            summary,
            upload,
            refresh,
            tbl,
        ],
    )
}

// =============================================================================
// Zakladka 3 — Czat + cytaty
// =============================================================================

fn chat_tab() -> Component {
    let mut collection_select = Select {
        bind_path: state_path(SP_CHAT_COLLECTION),
        options: collection_select_options(),
        placeholder: Some(lit("Wybierz kolekcje")),
        label: Some(lit("Kolekcja")),
        searchable: true,
        clearable: false,
        virtualize: false,
        disabled: None,
        size: InputSize::Md,
        groups: None,
    }
    .into_component("chat-collection")
    .expect("kodowanie Select");
    collection_select.handlers = Some(HandlerMap(vec![set_field_handler(
        EventKind::Change,
        SP_CHAT_COLLECTION,
    )]));

    let mut question = Textarea {
        bind_path: state_path(SP_CHAT_QUESTION),
        placeholder: Some(lit("Zadaj pytanie do kolekcji...")),
        label: Some(lit("Pytanie")),
        hint: None,
        validators: vec![],
        max_length: Some(4096),
        min_length: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
        rows: 3,
        autoresize: true,
        max_rows: Some(8),
        monospace: false,
    }
    .into_component("chat-question")
    .expect("kodowanie Textarea");
    question.handlers = Some(HandlerMap(vec![set_field_handler(
        EventKind::Change,
        SP_CHAT_QUESTION,
    )]));

    let ask = action_button("chat-ask", "Zapytaj", "ask-question", ButtonVariant::Primary, Tone::Neutral);

    // Odpowiedz jako Markdown (LLM zwraca tekst/markdown).
    let answer = Markdown {
        content: bound(SP_CHAT_ANSWER),
        allowed_features: vec![
            MarkdownFeature::Heading,
            MarkdownFeature::List,
            MarkdownFeature::CodeBlock,
            MarkdownFeature::CodeInline,
            MarkdownFeature::Blockquote,
            MarkdownFeature::Emphasis,
            MarkdownFeature::Strong,
            MarkdownFeature::Link,
        ],
        max_height_px: None,
        link_target: LinkTarget::BlankViaCommand,
    }
    .into_component("chat-answer")
    .expect("kodowanie Markdown");

    let citations = table(
        "chat-citations",
        SP_CITATION_ROWS,
        "key",
        vec![
            col("ci-doc", "Dokument", "doc_id", TableColumnWidth::Fr { value: 2 }),
            col("ci-chunk", "Chunk", "chunk_index", TableColumnWidth::Fr { value: 1 }),
            col("ci-score", "Wynik", "score", TableColumnWidth::Fr { value: 1 }),
            col("ci-text", "Pasaz", "text", TableColumnWidth::Fr { value: 5 }),
        ],
        vec![],
    );

    stack(
        "tab-chat",
        vec![
            heading("chat-heading", "Czat"),
            muted_caption("chat-status", bound(SP_STATUS_MESSAGE)),
            collection_select,
            question,
            ask,
            heading("chat-answer-heading", "Odpowiedz"),
            answer,
            heading("chat-cit-heading", "Cytaty"),
            citations,
        ],
    )
}

// =============================================================================
// Zakladka 4 — Graf (explorer)
// =============================================================================

fn graph_tab() -> Component {
    let query = text_input("graph-query", SP_GRAPH_QUERY, "Encja", "Nazwa encji (np. albert einstein)");
    let explore = action_button("graph-explore", "Eksploruj", "explore-graph", ButtonVariant::Primary, Tone::Neutral);
    let center = body_text("graph-center", bound(SP_GRAPH_CENTER));

    let neighbors = table(
        "graph-neighbors",
        SP_NEIGHBOR_ROWS,
        "key",
        vec![
            col("n-name", "Sasiad", "name", TableColumnWidth::Fr { value: 3 }),
            col("n-rel", "Relacja", "rel", TableColumnWidth::Fr { value: 2 }),
            col("n-weight", "Waga", "weight", TableColumnWidth::Fr { value: 1 }),
        ],
        vec![row_action("n-open", "Wejdz", "explore-neighbor", Tone::Primary)],
    );

    let facts = table(
        "graph-facts",
        SP_FACT_ROWS,
        "fact_key",
        vec![
            col("f-src", "Zrodlo", "source", TableColumnWidth::Fr { value: 2 }),
            col("f-rel", "Relacja", "rel", TableColumnWidth::Fr { value: 2 }),
            col("f-tgt", "Cel", "target", TableColumnWidth::Fr { value: 2 }),
            col("f-prov", "Provenance (dok)", "provenance_document_id", TableColumnWidth::Fr { value: 2 }),
        ],
        vec![],
    );

    stack(
        "tab-graph",
        vec![
            heading("graph-heading", "Graf wiedzy"),
            muted_caption("graph-status", bound(SP_STATUS_MESSAGE)),
            query,
            explore,
            center,
            heading("graph-neighbors-heading", "Sasiedztwo"),
            neighbors,
            heading("graph-facts-heading", "Fakty"),
            facts,
        ],
    )
}

// =============================================================================
// Zakladka 5 — Konflikty (panel D7)
// =============================================================================

fn conflicts_tab() -> Component {
    let mut status_select = Select {
        bind_path: state_path(SP_CONFLICT_STATUS),
        options: conflict_status_options(),
        placeholder: Some(lit("Status")),
        label: Some(lit("Filtr statusu")),
        searchable: false,
        clearable: false,
        virtualize: false,
        disabled: None,
        size: InputSize::Md,
        groups: None,
    }
    .into_component("conf-status")
    .expect("kodowanie Select status");
    status_select.handlers = Some(HandlerMap(vec![set_field_handler(
        EventKind::Change,
        SP_CONFLICT_STATUS,
    )]));

    let refresh = action_button("conf-refresh", "Filtruj", "filter-conflicts", ButtonVariant::Secondary, Tone::Neutral);

    // Reczne wyzwolenie agentow (admin).
    let scan = action_button("conf-scan", "A_det: skan", "run-conflict-scan", ButtonVariant::Ghost, Tone::Neutral);
    let resolve = action_button("conf-resolve", "A_res: adjudykuj", "run-conflict-resolve", ButtonVariant::Ghost, Tone::Neutral);
    let merge = action_button("conf-merge", "A_uni: scal encje", "run-entity-merge-scan", ButtonVariant::Ghost, Tone::Neutral);
    let admin_row = Stack {
        gap: Spacing::Sm,
        align: FlexAlign::Center,
        children: vec![scan, resolve, merge],
        padding: None,
    }
    .into_component("conf-admin-row")
    .expect("kodowanie Stack admin");

    let tbl = table(
        "conf-table",
        SP_CONFLICT_ROWS,
        "dedup_key",
        vec![
            col("k-type", "Typ", "conflict_type", TableColumnWidth::Fr { value: 2 }),
            col("k-head", "Encja (head)", "head_id", TableColumnWidth::Fr { value: 2 }),
            col("k-rel", "Relacja", "rel", TableColumnWidth::Fr { value: 2 }),
            col("k-members", "Fakty", "member_count", TableColumnWidth::Fr { value: 1 }),
            col("k-status", "Status", "status", TableColumnWidth::Fr { value: 2 }),
            col("k-decision", "Decyzja", "decision_action", TableColumnWidth::Fr { value: 2 }),
        ],
        vec![
            row_action("k-detail", "Szczegoly", "conflict-detail", Tone::Primary),
            row_action("k-approve", "Zatwierdz (escalated)", "approve-escalated", Tone::Success),
        ],
    );

    let detail = body_text("conf-detail", bound(SP_CONFLICT_DETAIL));

    stack(
        "tab-conflicts",
        vec![
            heading("conf-heading", "Konflikty"),
            muted_caption("conf-status-msg", bound(SP_STATUS_MESSAGE)),
            status_select,
            refresh,
            admin_row,
            tbl,
            heading("conf-detail-heading", "Szczegoly konfliktu"),
            detail,
        ],
    )
}

// =============================================================================
// Ladowanie danych (wiersze tabel) — wolane przez read tooly z lib.rs
// =============================================================================

/// Wybor kolekcji/filtr trzymamy w KV scope'owanym na biezaca sesje panelu
/// (`session_key`), by przetrwaly miedzy wywolaniami on_request — statyki WASM nie
/// sa niezawodnie wspoldzielone miedzy wywolaniami modulu w runtime hosta. Klucz
/// per-sesja => wybor jednego usera nie nadpisuje wyboru innego na tej instancji.
fn selected_collection() -> String {
    crate::state_get(&session_key(SP_SELECTED_COLLECTION))
        .ok()
        .flatten()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}

fn conflict_status_filter() -> String {
    crate::state_get(&session_key(SP_CONFLICT_STATUS))
        .ok()
        .flatten()
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "open".to_string())
}

fn cbor_from_json(v: &JsonValue) -> CborValue {
    match v {
        JsonValue::Null => CborValue::Null,
        JsonValue::Bool(b) => CborValue::Bool(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                CborValue::I64(i)
            } else if let Some(u) = n.as_u64() {
                CborValue::U64(u)
            } else {
                CborValue::F64(n.as_f64().unwrap_or(0.0))
            }
        }
        JsonValue::String(s) => CborValue::Text(s.clone()),
        JsonValue::Array(a) => CborValue::Array(a.iter().map(cbor_from_json).collect()),
        JsonValue::Object(o) => CborValue::Map(
            o.iter()
                .map(|(k, val)| (CborValue::Text(k.clone()), cbor_from_json(val)))
                .collect(),
        ),
    }
}

fn rows_to_cbor(rows: Vec<JsonValue>) -> CborValue {
    CborValue::Array(rows.iter().map(cbor_from_json).collect())
}

fn load_collection_rows() -> CborValue {
    let res = crate::handle_list_collections();
    let cols = res
        .get("data")
        .and_then(|d| d.get("collections"))
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let rows: Vec<JsonValue> = cols
        .iter()
        .map(|c| {
            json!({
                "id": c.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "name": c.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "document_count": c.get("document_count").and_then(|v| v.as_i64()).unwrap_or(0),
                "created": fmt_ts(c.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0)),
            })
        })
        .collect();
    rows_to_cbor(rows)
}

fn load_document_rows(collection_id: &str) -> CborValue {
    let res = crate::handle_list_documents(&json!({ "collection_id": collection_id }));
    let docs = res
        .get("data")
        .and_then(|d| d.get("documents"))
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let rows: Vec<JsonValue> = docs
        .iter()
        .map(|d| {
            let partial = d.get("graph_partial").and_then(|v| v.as_bool()).unwrap_or(false);
            json!({
                "id": d.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "filename": d.get("filename").and_then(|v| v.as_str()).unwrap_or(""),
                "mime": d.get("mime").and_then(|v| v.as_str()).unwrap_or(""),
                "chunk_count": d.get("chunk_count").and_then(|v| v.as_i64()).unwrap_or(0),
                "entity_count": d.get("entity_count").and_then(|v| v.as_i64()).unwrap_or(0),
                "relation_count": d.get("relation_count").and_then(|v| v.as_i64()).unwrap_or(0),
                "graph_flag": if partial { "czesciowy" } else { "pelny" },
                "status": d.get("status").and_then(|v| v.as_str()).unwrap_or(""),
            })
        })
        .collect();
    rows_to_cbor(rows)
}

fn load_ingest_summary(collection_id: &str) -> String {
    let res = crate::handle_collection_ingest_status(&json!({ "collection_id": collection_id }));
    let d = match res.get("data") {
        Some(d) => d,
        None => return "Brak danych statusu ingestu.".to_string(),
    };
    format!(
        "Dokumenty: {} | gotowe: {} | w toku: {} | bledy: {} | graf czesciowy: {}",
        d.get("total").and_then(|v| v.as_i64()).unwrap_or(0),
        d.get("ingested").and_then(|v| v.as_i64()).unwrap_or(0),
        d.get("pending").and_then(|v| v.as_i64()).unwrap_or(0),
        d.get("failed").and_then(|v| v.as_i64()).unwrap_or(0),
        d.get("graph_partial").and_then(|v| v.as_i64()).unwrap_or(0),
    )
}

fn load_conflict_rows(status: &str) -> CborValue {
    let mut params = json!({ "limit": 100 });
    if !status.is_empty() && status != "all" {
        params["status"] = json!(status);
    }
    let res = crate::handle_list_conflicts(&params);
    let conflicts = res
        .get("data")
        .and_then(|d| d.get("conflicts"))
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let rows: Vec<JsonValue> = conflicts
        .iter()
        .map(|c| {
            let decision_action = c
                .get("decision")
                .and_then(|d| d.get("action"))
                .and_then(|a| a.as_str())
                .unwrap_or("-");
            json!({
                "dedup_key": c.get("dedup_key").and_then(|v| v.as_str()).unwrap_or(""),
                "id": c.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                "conflict_type": c.get("conflict_type").and_then(|v| v.as_str()).unwrap_or(""),
                "head_id": c.get("head_id").and_then(|v| v.as_str()).unwrap_or(""),
                "rel": c.get("rel").and_then(|v| v.as_str()).unwrap_or(""),
                "member_count": c.get("member_count").and_then(|v| v.as_i64()).unwrap_or(0),
                "status": c.get("status").and_then(|v| v.as_str()).unwrap_or(""),
                "decision_action": decision_action,
            })
        })
        .collect();
    rows_to_cbor(rows)
}

/// Opcje Select kolekcji (czat) — z handle_list_collections.
fn collection_select_options() -> Vec<SelectOption> {
    let res = crate::handle_list_collections();
    res.get("data")
        .and_then(|d| d.get("collections"))
        .and_then(|c| c.as_array())
        .map(|cols| {
            cols.iter()
                .filter_map(|c| {
                    let id = c.get("id").and_then(|v| v.as_str())?;
                    let name = c.get("name").and_then(|v| v.as_str()).unwrap_or(id);
                    Some(SelectOption {
                        value: SelectValue::Text(id.to_string()),
                        label: lit(name),
                        icon: None,
                        disabled: false,
                        group_id: None,
                        description: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn conflict_status_options() -> Vec<SelectOption> {
    let opt = |value: &str, label: &str| SelectOption {
        value: SelectValue::Text(value.to_string()),
        label: lit(label),
        icon: None,
        disabled: false,
        group_id: None,
        description: None,
    };
    vec![
        opt("open", "Otwarte"),
        opt("resolving", "W trakcie"),
        opt("escalated", "Eskalowane"),
        opt("resolved_auto", "Rozwiazane (auto)"),
        opt("resolved_merge_pending", "Do scalenia"),
        opt("all", "Wszystkie"),
    ]
}

/// Skrocony format znacznika czasu (sekundy uniksowe -> ISO-podobny tekst). Bez
/// zewnetrznych crateow daty — w GUI wystarczy sekundowy epoch jako liczba dni/godzin.
fn fmt_ts(unix: i64) -> String {
    if unix <= 0 {
        return "-".to_string();
    }
    // Prosty rozklad na YYYY-MM-DD HH:MM (UTC) bez crate'ow — wystarczajacy do listy.
    let days = unix / 86_400;
    let secs_of_day = unix % 86_400;
    let (h, m) = (secs_of_day / 3600, (secs_of_day % 3600) / 60);
    // Liczymy date od 1970-01-01 (algorytm cywilny, proleptyczny gregorianski).
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}")
}

/// Konwersja liczby dni od epoki na (rok, miesiac, dzien) — algorytm Howarda Hinnanta.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// =============================================================================
// Akcje UI — host wola "ui.main.<action>" -> tu -> read/write tool -> StatePatch
// =============================================================================

pub fn handle_ui_action(action_id: &str, params: &JsonValue) -> JsonValue {
    match action_id {
        "set-field" => action_set_field(params),
        "ingest-uploaded" => action_ingest_uploaded(params),
        "panel-navigate" => {
            let tab = params
                .get("item_id")
                .or_else(|| params.get("panel_id"))
                .or_else(|| params.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or(DEFAULT_TAB)
                .to_string();
            send_tab_content(&tab);
            json!({"ok": true, "tab": tab})
        }
        "refresh-collections" => {
            send_tab_content(TAB_COLLECTIONS);
            json!({"ok": true})
        }
        "create-collection" => action_create_collection(params),
        "delete-collection" => action_delete_collection(params),
        "open-collection" => action_open_collection(params),
        "refresh-documents" => {
            send_tab_content(TAB_DOCUMENTS);
            json!({"ok": true})
        }
        "delete-document" => action_delete_document(params),
        "ask-question" => action_ask(params),
        "explore-graph" => action_explore_graph(params, false),
        "explore-neighbor" => action_explore_graph(params, true),
        "filter-conflicts" => {
            // Wartosc selecta zostala juz zapisana do KV przez `set-field` (on-change);
            // filtr tylko przerenderowuje zakladke wg aktualnego SP_CONFLICT_STATUS.
            send_tab_content(TAB_CONFLICTS);
            json!({"ok": true})
        }
        "conflict-detail" => action_conflict_detail(params),
        "approve-escalated" => action_approve_escalated(params),
        "run-conflict-scan" => action_run_agent(params, "conflict_scan", "A_det skan"),
        "run-conflict-resolve" => action_run_agent(params, "conflict_resolve", "A_res adjudykacja"),
        "run-entity-merge-scan" => action_run_agent(params, "entity_merge_scan", "A_uni scalanie"),
        other => json!({"ok": true, "ignored": other}),
    }
}

/// Zapis wartosci pola formularza do KV scope'owanego na biezaca sesje panelu
/// (`f:{user}:{epoch}:{field}`). Ephemeral: wartosci pol to stan jednej sesji UI,
/// nie ma sensu utrwalac ich na dysku ani dzielic miedzy sesjami — RAM-only znika
/// z instancja i nie zostawia osieroconych wpisow po restarcie.
fn set_kv(field: &str, value: &str) {
    let _ = crate::state_set(
        &session_key(field),
        value.as_bytes(),
        crate::StateTier::Ephemeral,
    );
}

/// Odczyt zebranej wartosci pola formularza z KV biezacej sesji (zapisanej przez
/// `set-field` on-change). Submit czyta wartosci STAD, bo klik submita nie niesie
/// wartosci inputow. Pusty/biały ciag -> None. Klucz per-sesja => brak przeciekow.
fn field_value(field: &str) -> Option<String> {
    crate::state_get(&session_key(field))
        .ok()
        .flatten()
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Akcja `set-field`: zapisuje wartosc pola (input/textarea/select) do KV pod
/// kluczem `field` (z handler.params). Wartosc przychodzi w `value` (detail eventu
/// `change`). Pusty `value` tez zapisujemy (czysci pole) — dzieki temu skasowanie
/// tresci jest widoczne dla submita. NIE przerenderowuje panelu (bind reaktywny
/// trzyma widok inputu; re-render zabilby focus przy kazdym keystroke/commit).
fn action_set_field(params: &JsonValue) -> JsonValue {
    let field = match params.get("field").and_then(|v| v.as_str()) {
        Some(f) if !f.is_empty() => f,
        None => return json!({"ok": false, "error": "brak 'field'"}),
        _ => return json!({"ok": false, "error": "puste 'field'"}),
    };
    if !is_known_field(field) {
        return json!({"ok": false, "error": format!("nieznane pole: {field}")});
    }
    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
    set_kv(field, value.trim());
    json!({"ok": true, "field": field})
}

/// Allowlista pol formularza, ktore `set-field` moze zapisac do KV. Zapobiega
/// zapisaniu dowolnego klucza KV przez spreparowany action param.
fn is_known_field(field: &str) -> bool {
    matches!(
        field,
        SP_NEW_COLLECTION
            | SP_CHAT_QUESTION
            | SP_CHAT_COLLECTION
            | SP_GRAPH_QUERY
            | SP_CONFLICT_STATUS
    )
}

/// Czysty parser detailu eventu `upload_complete` -> (doc_ref, mime, filename).
/// Renderer emituje `{doc_ref, filename, mime, name, size}`; `name` jest aliasem
/// `filename`. Zwraca czytelny blad gdy brak wymaganych pol.
fn parse_upload_detail(params: &JsonValue) -> Result<(String, String, String), String> {
    let doc_ref = params
        .get("doc_ref")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Upload nie zwrocil referencji pliku (doc_ref).".to_string())?;
    let mime = params
        .get("mime")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Upload bez typu MIME pliku.".to_string())?;
    let filename = params
        .get("filename")
        .or_else(|| params.get("name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("dokument");
    Ok((doc_ref.to_string(), mime.to_string(), filename.to_string()))
}

/// Akcja `ingest-uploaded`: wpiecie uploadu (upload_complete -> ingest). FileInput
/// emituje `upload_complete` z detail `{doc_ref, filename, mime, name, size}` PO
/// chunked-uploadzie hosta do document store; `doc_ref` to id bloba czytelny przez
/// document_get. Uruchamiamy pelny ingest na wybranej kolekcji (parse->chunk->embedding).
fn action_ingest_uploaded(params: &JsonValue) -> JsonValue {
    let collection = selected_collection();
    if collection.is_empty() {
        patch_status("Najpierw otworz kolekcje (zakladka Kolekcje -> Otworz).");
        return json!({"ok": false, "error": "brak wybranej kolekcji"});
    }
    let (doc_ref, mime, filename) = match parse_upload_detail(params) {
        Ok(parts) => parts,
        Err(msg) => {
            patch_status(&msg);
            return json!({"ok": false, "error": msg});
        }
    };

    patch_status(&format!("Ingest pliku '{filename}'..."));
    let res = crate::handle_ingest_document(&json!({
        "collection_id": collection,
        "doc_id_blob": doc_ref,
        "filename": filename,
        "mime": mime,
    }));
    if res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let chunks = res
            .get("data")
            .and_then(|d| d.get("chunks"))
            .and_then(|c| c.as_i64())
            .unwrap_or(0);
        patch_status(&format!("Zingestowano '{filename}' ({chunks} chunkow)."));
        send_tab_content(TAB_DOCUMENTS);
    } else {
        patch_status(&format!("Ingest '{filename}': {}", error_text(&res)));
        // Odswiez liste (dokument moze byc w stanie failed z artefaktami statusu).
        send_tab_content(TAB_DOCUMENTS);
    }
    res
}

fn action_create_collection(_params: &JsonValue) -> JsonValue {
    // Wartosc inputu czytamy z KV (zapisana przez `set-field`), bo klik submita
    // nie niesie wartosci pola.
    let name = match field_value(SP_NEW_COLLECTION) {
        Some(n) => n,
        None => {
            patch_status("Podaj nazwe kolekcji.");
            return json!({"ok": false, "error": "brak nazwy"});
        }
    };
    let res = crate::handle_create_collection(&json!({ "name": name }));
    if res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        patch_status(&format!("Utworzono kolekcje '{name}'."));
        // Wyczysc pole nazwy w KV i w widoku po udanym utworzeniu.
        set_kv(SP_NEW_COLLECTION, "");
        patch_set(SP_NEW_COLLECTION, CborValue::Text("".into()));
        send_tab_content(TAB_COLLECTIONS);
    } else {
        patch_status(&error_text(&res));
    }
    res
}

fn action_delete_collection(params: &JsonValue) -> JsonValue {
    let id = match row_key(params, "id") {
        Some(id) => id,
        None => return json!({"ok": false, "error": "brak collection_id"}),
    };
    let res = crate::handle_delete_collection(&json!({ "collection_id": id }));
    if res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        patch_status("Usunieto kolekcje.");
        send_tab_content(TAB_COLLECTIONS);
    } else {
        patch_status(&error_text(&res));
    }
    res
}

fn action_open_collection(params: &JsonValue) -> JsonValue {
    let id = match row_key(params, "id") {
        Some(id) => id,
        None => return json!({"ok": false, "error": "brak collection_id"}),
    };
    // Nazwa kolekcji do podgladu w naglowku zakladki Dokumenty.
    let name = collection_name(&id).unwrap_or_else(|| id.clone());
    set_kv(SP_SELECTED_COLLECTION, &id);
    set_kv(SP_SELECTED_COLLECTION_NAME, &name);
    patch_set(SP_SELECTED_COLLECTION, CborValue::Text(id.clone()));
    patch_set(SP_SELECTED_COLLECTION_NAME, CborValue::Text(format!("Kolekcja: {name}")));
    send_tab_content(TAB_DOCUMENTS);
    json!({"ok": true, "collection_id": id})
}

fn action_delete_document(params: &JsonValue) -> JsonValue {
    let id = match row_key(params, "id") {
        Some(id) => id,
        None => return json!({"ok": false, "error": "brak document_id"}),
    };
    let res = crate::handle_delete_document(&json!({ "document_id": id }));
    if res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        patch_status("Usunieto dokument.");
        send_tab_content(TAB_DOCUMENTS);
    } else {
        patch_status(&error_text(&res));
    }
    res
}

fn action_ask(_params: &JsonValue) -> JsonValue {
    // Pytanie i kolekcja czytane z KV (zapisane przez `set-field` on-change).
    let question = match field_value(SP_CHAT_QUESTION) {
        Some(q) => q,
        None => {
            patch_status("Wpisz pytanie.");
            return json!({"ok": false, "error": "brak pytania"});
        }
    };
    let collection = field_value(SP_CHAT_COLLECTION).or_else(|| {
        let s = selected_collection();
        if s.is_empty() { None } else { Some(s) }
    });
    let collection = match collection {
        Some(c) => c,
        None => {
            patch_status("Wybierz kolekcje do pytania.");
            return json!({"ok": false, "error": "brak kolekcji"});
        }
    };

    patch_status("Pytanie w toku...");
    let res = crate::handle_ask(&json!({ "collection_id": collection, "question": question }));
    if !res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        patch_status(&error_text(&res));
        patch_set(SP_CHAT_ANSWER, CborValue::Text("".into()));
        patch_set(SP_CITATION_ROWS, CborValue::Array(vec![]));
        return res;
    }
    let answer = res
        .get("data")
        .and_then(|d| d.get("answer"))
        .and_then(|a| a.as_str())
        .unwrap_or("");
    let citations = res
        .get("data")
        .and_then(|d| d.get("citations"))
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let rows: Vec<JsonValue> = citations
        .iter()
        .enumerate()
        .map(|(i, c)| {
            json!({
                "key": format!("cit-{i}"),
                "doc_id": c.get("doc_id").and_then(|v| v.as_str()).unwrap_or(""),
                "chunk_index": c.get("chunk_index").and_then(|v| v.as_i64()).unwrap_or(0),
                "score": fmt_score(c.get("score")),
                "text": truncate_chars(c.get("text").and_then(|v| v.as_str()).unwrap_or(""), 300),
            })
        })
        .collect();
    patch_set(SP_CHAT_ANSWER, CborValue::Text(answer.to_string()));
    patch_set(SP_CITATION_ROWS, rows_to_cbor(rows));
    patch_status(&format!("Gotowe — {} cytatow.", citations.len()));
    res
}

fn action_explore_graph(params: &JsonValue, from_neighbor: bool) -> JsonValue {
    // Z sasiada bierzemy node_id (klucz wiersza), z pola tekstowego entity_query.
    let req = if from_neighbor {
        match row_key(params, "key").or_else(|| row_key(params, "id")) {
            Some(node_id) => json!({ "node_id": node_id }),
            None => return json!({"ok": false, "error": "brak node_id sasiada"}),
        }
    } else {
        // Zapytanie tekstowe czytane z KV (zapisane przez `set-field` on-change).
        match field_value(SP_GRAPH_QUERY) {
            Some(q) => json!({ "entity_query": q }),
            None => {
                patch_status("Wpisz nazwe encji.");
                return json!({"ok": false, "error": "brak zapytania"});
            }
        }
    };

    let res = crate::handle_graph_explore(&req);
    if !res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        patch_status(&error_text(&res));
        patch_set(SP_GRAPH_CENTER, CborValue::Text("".into()));
        patch_set(SP_NEIGHBOR_ROWS, CborValue::Array(vec![]));
        patch_set(SP_FACT_ROWS, CborValue::Array(vec![]));
        return res;
    }
    let data = res.get("data").cloned().unwrap_or(json!({}));
    let center_name = data
        .get("center")
        .and_then(|c| c.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");
    let neighbors = data.get("neighbors").and_then(|n| n.as_array()).cloned().unwrap_or_default();
    let facts = data.get("facts").and_then(|f| f.as_array()).cloned().unwrap_or_default();

    let neighbor_rows: Vec<JsonValue> = neighbors
        .iter()
        .map(|n| {
            json!({
                "key": n.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "name": n.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "rel": n.get("rel").and_then(|v| v.as_str()).unwrap_or(""),
                "weight": fmt_score(n.get("weight")),
            })
        })
        .collect();
    let fact_rows: Vec<JsonValue> = facts
        .iter()
        .map(|f| {
            json!({
                "fact_key": f.get("fact_key").and_then(|v| v.as_str()).unwrap_or(""),
                "source": f.get("source").and_then(|v| v.as_str()).unwrap_or(""),
                "rel": f.get("rel").and_then(|v| v.as_str()).unwrap_or(""),
                "target": f.get("target").and_then(|v| v.as_str()).unwrap_or(""),
                "provenance_document_id": f.get("provenance_document_id").and_then(|v| v.as_str()).unwrap_or("-"),
            })
        })
        .collect();

    patch_set(SP_GRAPH_CENTER, CborValue::Text(format!("Encja: {center_name}")));
    patch_set(SP_NEIGHBOR_ROWS, rows_to_cbor(neighbor_rows));
    patch_set(SP_FACT_ROWS, rows_to_cbor(fact_rows));
    patch_status(&format!("{} sasiadow, {} faktow.", neighbors.len(), facts.len()));
    res
}

fn action_conflict_detail(params: &JsonValue) -> JsonValue {
    let dedup_key = match row_key(params, "dedup_key") {
        Some(k) => k,
        None => return json!({"ok": false, "error": "brak dedup_key"}),
    };
    let res = crate::handle_conflict_detail(&json!({ "dedup_key": dedup_key }));
    if !res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        patch_status(&error_text(&res));
        return res;
    }
    let d = res.get("data").cloned().unwrap_or(json!({}));
    patch_set(SP_CONFLICT_DETAIL, CborValue::Text(format_conflict_detail(&d)));
    patch_status("Wczytano szczegoly konfliktu.");
    res
}

fn action_approve_escalated(params: &JsonValue) -> JsonValue {
    // Reczne zatwierdzenie eskalowanego: zwyciezca = pierwszy aktywny fakt grupy
    // (keep_winner). Gdy grupa nie ma jednoznacznego zwyciezcy, admin uzywa
    // szczegolow + recznego wywolania resolve_escalated; tu szybka sciezka.
    let dedup_key = match row_key(params, "dedup_key") {
        Some(k) => k,
        None => return json!({"ok": false, "error": "brak dedup_key"}),
    };
    let detail = crate::handle_conflict_detail(&json!({ "dedup_key": dedup_key }));
    let data = detail.get("data").cloned().unwrap_or(json!({}));
    let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "escalated" {
        patch_status("Zatwierdzic mozna tylko konflikt eskalowany.");
        return json!({"ok": false, "error": "konflikt nie jest eskalowany"});
    }
    let conflict_id = data.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let winner = data
        .get("members")
        .and_then(|m| m.as_array())
        .and_then(|arr| arr.iter().find(|f| f.get("active").and_then(|a| a.as_bool()).unwrap_or(false)))
        .and_then(|f| f.get("fact_key").and_then(|k| k.as_str()))
        .map(|s| s.to_string());
    let winner = match winner {
        Some(w) => w,
        None => {
            patch_status("Brak aktywnego faktu do uznania za zwyciezce.");
            return json!({"ok": false, "error": "brak zwyciezcy"});
        }
    };
    let res = crate::handle_resolve_escalated(&json!({
        "conflict_id": conflict_id,
        "action": "keep_winner",
        "winner_fact_key": winner,
    }));
    if res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        patch_status("Konflikt rozstrzygniety (keep_winner).");
        send_tab_content(TAB_CONFLICTS);
    } else {
        patch_status(&error_text(&res));
    }
    res
}

fn action_run_agent(_params: &JsonValue, tool: &str, label: &str) -> JsonValue {
    let res = match tool {
        "conflict_scan" => crate::handle_conflict_scan(&json!({})),
        "conflict_resolve" => crate::handle_conflict_resolve(&json!({})),
        "entity_merge_scan" => crate::handle_entity_merge_scan(&json!({})),
        _ => json!({"ok": false, "error": "nieznany agent"}),
    };
    if res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        patch_status(&format!("{label}: wykonano."));
        send_tab_content(TAB_CONFLICTS);
    } else {
        patch_status(&format!("{label}: {}", error_text(&res)));
    }
    res
}

// =============================================================================
// Pomocnicze: klucz wiersza, formatowanie, StatePatch
// =============================================================================

/// Klucz wiersza tabeli przekazany przez host w params (pod nazwa row_key_field
/// albo generycznymi "row_key"/"id"/"key").
fn row_key(params: &JsonValue, field: &str) -> Option<String> {
    params
        .get(field)
        .or_else(|| params.get("row_key"))
        .or_else(|| params.get("id"))
        .or_else(|| params.get("key"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn collection_name(id: &str) -> Option<String> {
    let res = crate::handle_list_collections();
    res.get("data")
        .and_then(|d| d.get("collections"))
        .and_then(|c| c.as_array())
        .and_then(|cols| {
            cols.iter()
                .find(|c| c.get("id").and_then(|v| v.as_str()) == Some(id))
                .and_then(|c| c.get("name").and_then(|v| v.as_str()).map(str::to_string))
        })
}

fn error_text(res: &JsonValue) -> String {
    res.get("error")
        .and_then(|e| e.as_str())
        .unwrap_or("Nieznany blad")
        .to_string()
}

fn fmt_score(v: Option<&JsonValue>) -> String {
    match v.and_then(|x| x.as_f64()) {
        Some(f) => format!("{f:.3}"),
        None => "-".to_string(),
    }
}

fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn format_conflict_detail(d: &JsonValue) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Typ: {} | head: {} | rel: {} | status: {}\n",
        d.get("conflict_type").and_then(|v| v.as_str()).unwrap_or("-"),
        d.get("head_id").and_then(|v| v.as_str()).unwrap_or("-"),
        d.get("rel").and_then(|v| v.as_str()).unwrap_or("-"),
        d.get("status").and_then(|v| v.as_str()).unwrap_or("-"),
    ));
    if let Some(action) = d.get("decision").and_then(|x| x.get("action")).and_then(|a| a.as_str()) {
        out.push_str(&format!("Decyzja A_res: {action}\n"));
    }
    if let Some(members) = d.get("members").and_then(|m| m.as_array()) {
        out.push_str(&format!("\nFakty ({}):\n", members.len()));
        for m in members {
            let active = m.get("active").and_then(|a| a.as_bool()).unwrap_or(false);
            out.push_str(&format!(
                " - {} -[{}]-> {} [{}]\n",
                m.get("head_id").and_then(|v| v.as_str()).unwrap_or(""),
                m.get("rel").and_then(|v| v.as_str()).unwrap_or(""),
                m.get("tail_id").and_then(|v| v.as_str()).unwrap_or(""),
                if active { "aktywny" } else { "nieaktywny" },
            ));
            if let Some(ev) = m.get("evidence").and_then(|e| e.as_array()) {
                for e in ev.iter().take(2) {
                    let passage = e.get("passage").and_then(|v| v.as_str()).unwrap_or("");
                    out.push_str(&format!("     dowod: {}\n", truncate_chars(passage, 160)));
                }
            }
        }
    }
    out
}

/// Komunikat statusu w panelu (caption widoczny w kazdej zakladce).
fn patch_status(text: &str) {
    patch_set(SP_STATUS_MESSAGE, CborValue::Text(text.into()));
}

/// Pojedynczy StatePatch ustawiajacy jedna sciezke. Rewizje advance'ujemy tylko gdy
/// host zaakceptowal patch (jak sdk-showcase) — inaczej liczniki rozjechalyby sie.
fn patch_set(key: &str, value: CborValue) {
    send_state_patch(vec![PatchOp {
        path: state_path(key),
        op: PatchOpKind::Set { value },
    }]);
}

fn send_state_patch(ops: Vec<PatchOp>) {
    let base = unsafe { STATE_REVISION };
    let new = base + 1;
    let patch = StatePatch {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: panel_epoch(),
        base_revision: base,
        new_revision: new,
        ops,
    };
    if send_ui(&UiPayload::StatePatch(patch)) == 0 {
        unsafe {
            STATE_REVISION = new;
        }
    }
}

// =============================================================================
// Testy CZYSTYCH helperow UI (bez host-fn): formatowanie, mapowanie JSON->CBOR,
// odczyt kluczy wierszy i parametrow formularzy.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_known_dates() {
        // 1970-01-01 = dzien 0.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-03-01 = dzien 11017 (po przestepnym lutym 2000).
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
    }

    #[test]
    fn fmt_ts_formats_epoch_and_handles_zero() {
        assert_eq!(fmt_ts(0), "-");
        assert_eq!(fmt_ts(-5), "-");
        // 1700000000 = 2023-11-14 22:13 UTC.
        let s = fmt_ts(1_700_000_000);
        assert!(s.starts_with("2023-11-14"), "got: {s}");
    }

    #[test]
    fn fmt_score_rounds_and_handles_missing() {
        assert_eq!(fmt_score(Some(&json!(0.123456))), "0.123");
        assert_eq!(fmt_score(Some(&json!(1))), "1.000");
        assert_eq!(fmt_score(None), "-");
        assert_eq!(fmt_score(Some(&json!("nie-liczba"))), "-");
    }

    #[test]
    fn truncate_chars_respects_utf8_boundaries() {
        // Liczymy ZNAKI (nie bajty) — bez przeciecia wielobajtowego znaku.
        let s = "ąęółść-dlugi-tekst";
        let t = truncate_chars(s, 4);
        assert_eq!(t.chars().count(), 4);
        assert_eq!(t, "ąęół");
    }

    #[test]
    fn is_known_field_allows_form_fields_only() {
        // Allowlista pol formularza zapisywanych przez `set-field`.
        for f in [
            SP_NEW_COLLECTION,
            SP_CHAT_QUESTION,
            SP_CHAT_COLLECTION,
            SP_GRAPH_QUERY,
            SP_CONFLICT_STATUS,
        ] {
            assert!(is_known_field(f), "pole {f} powinno byc dozwolone");
        }
        // Klucze stanu NIE bedace polami formularza (np. wybrana kolekcja, wiersze) sa
        // odrzucane — set-field nie moze pisac dowolnego klucza KV.
        assert!(!is_known_field(SP_SELECTED_COLLECTION));
        assert!(!is_known_field(SP_COLLECTION_ROWS));
        assert!(!is_known_field("dowolny_inny_klucz"));
    }

    #[test]
    fn parse_upload_detail_reads_complete_event_shape() {
        // Detail eventu upload_complete: {doc_ref, filename, mime, name, size}.
        let d = json!({
            "doc_ref": "blob-123",
            "filename": "raport.pdf",
            "mime": "application/pdf",
            "name": "raport.pdf",
            "size": 4096
        });
        let (doc_ref, mime, filename) = parse_upload_detail(&d).expect("poprawny detail");
        assert_eq!(doc_ref, "blob-123");
        assert_eq!(mime, "application/pdf");
        assert_eq!(filename, "raport.pdf");
    }

    #[test]
    fn parse_upload_detail_falls_back_to_name_and_default_filename() {
        // Brak `filename`, jest `name` -> uzywamy name.
        let d = json!({"doc_ref": "b", "mime": "text/plain", "name": "notatka.txt"});
        let (_, _, filename) = parse_upload_detail(&d).unwrap();
        assert_eq!(filename, "notatka.txt");
        // Brak nazwy w ogole -> "dokument".
        let d2 = json!({"doc_ref": "b", "mime": "text/plain"});
        let (_, _, filename2) = parse_upload_detail(&d2).unwrap();
        assert_eq!(filename2, "dokument");
    }

    #[test]
    fn parse_upload_detail_rejects_missing_doc_ref_or_mime() {
        // Brak doc_ref -> blad (nie da sie zingestowac bez referencji bloba).
        assert!(parse_upload_detail(&json!({"mime": "text/plain"})).is_err());
        // Pusty doc_ref tez odrzucony.
        assert!(parse_upload_detail(&json!({"doc_ref": "", "mime": "text/plain"})).is_err());
        // Brak mime -> blad (ingest wymaga MIME do klasyfikacji).
        assert!(parse_upload_detail(&json!({"doc_ref": "b"})).is_err());
    }

    #[test]
    fn row_key_reads_field_then_generic_fallbacks() {
        assert_eq!(row_key(&json!({"id": "doc1"}), "id"), Some("doc1".to_string()));
        // Generyczny fallback "row_key".
        assert_eq!(row_key(&json!({"row_key": "k"}), "dedup_key"), Some("k".to_string()));
        // Pusty -> None.
        assert_eq!(row_key(&json!({"id": ""}), "id"), None);
        assert_eq!(row_key(&json!({}), "id"), None);
    }

    #[test]
    fn cbor_from_json_maps_scalars_and_containers() {
        // Liczby calkowite -> I64/U64, ulamki -> F64, struktura zachowana.
        let v = json!({"a": 1, "b": 2.5, "c": [true, "x"], "d": null});
        match cbor_from_json(&v) {
            CborValue::Map(entries) => {
                assert_eq!(entries.len(), 4);
            }
            other => panic!("oczekiwano Map, jest {other:?}"),
        }
        assert!(matches!(cbor_from_json(&json!(7)), CborValue::I64(7)));
        assert!(matches!(cbor_from_json(&json!(2.5)), CborValue::F64(_)));
        assert!(matches!(cbor_from_json(&json!("t")), CborValue::Text(_)));
    }

    #[test]
    fn error_text_extracts_message_with_fallback() {
        assert_eq!(error_text(&json!({"error": "boom"})), "boom");
        assert_eq!(error_text(&json!({"ok": false})), "Nieznany blad");
    }

    #[test]
    fn session_key_isolates_users_and_epochs() {
        // Pola formularza musza byc kluczowane per-sesja, inaczej KV hosta (scope =
        // addon_id) przeciekaloby miedzy userami/sesjami tej samej instancji.
        // User A, epoch 1.
        set_panel_epoch(1);
        set_session_user(Some("user-A"));
        let a1 = session_key(SP_NEW_COLLECTION);
        // User B, ten sam epoch 1 — INNY klucz (izolacja userow).
        set_session_user(Some("user-B"));
        let b1 = session_key(SP_NEW_COLLECTION);
        assert_ne!(a1, b1, "rozni userzy nie moga dzielic klucza pola");

        // Ten sam user, INNY epoch (np. druga karta / reopen) — INNY klucz.
        set_session_user(Some("user-A"));
        set_panel_epoch(2);
        let a2 = session_key(SP_NEW_COLLECTION);
        assert_ne!(a1, a2, "rozne otwarcia panelu nie moga dzielic klucza pola");

        // Brak usera => sesja anonimowa, ale nadal odrebna od realnego usera.
        set_session_user(None);
        let anon = session_key(SP_NEW_COLLECTION);
        assert!(anon.contains("anon:"), "brak usera => 'anon': {anon}");
        assert_ne!(anon, a2);

        // Format klucza: prefix `f:` + sesja + pole (rozne pola => rozne klucze).
        set_session_user(Some("user-A"));
        set_panel_epoch(7);
        assert_eq!(session_key(SP_NEW_COLLECTION), "f:user-A:7:new_collection_name");
        assert_ne!(
            session_key(SP_CHAT_QUESTION),
            session_key(SP_GRAPH_QUERY),
            "rozne pola tej samej sesji musza miec rozne klucze"
        );
    }

    #[test]
    fn adopt_action_epoch_is_source_of_truth_for_session_key() {
        // Instancja pulowana: statyk zostawiony z poprzedniego wywolania jest cudzy.
        set_panel_epoch(99);
        set_session_user(Some("user-A"));
        let stale = session_key(SP_NEW_COLLECTION);
        assert_eq!(stale, "f:user-A:99:new_collection_name");

        // Akcja niesie epoch=5 -> adopcja przeklucza session_key na epoch akcji, nie statyk.
        adopt_action_epoch(5);
        assert_eq!(panel_epoch(), 5, "epoch akcji jest zrodlem prawdy");
        assert_eq!(session_key(SP_NEW_COLLECTION), "f:user-A:5:new_collection_name");

        // Ten sam user, dwie karty (rozne epoch z akcji) => rozne klucze pol (brak kolizji).
        adopt_action_epoch(6);
        let card_b = session_key(SP_NEW_COLLECTION);
        assert_ne!(
            session_key_for(5, "user-A", SP_NEW_COLLECTION),
            card_b,
            "dwie karty (rozne epoch akcji) nie moga dzielic klucza pola"
        );
    }

    /// Buduje oczekiwany klucz pola dla zadanego epocha/usera bez mutowania statykow.
    fn session_key_for(epoch: u64, user: &str, field: &str) -> String {
        format!("f:{user}:{epoch}:{field}")
    }

    #[test]
    fn adopt_action_epoch_same_epoch_preserves_revision() {
        // Adopcja TEGO SAMEGO epocha to NO-OP: licznik rewizji sesji nie moze sie zerowac
        // mid-sesja (host sledzi rewizje per-epoch; reset zlamalby base_revision patchy).
        set_panel_epoch(4);
        unsafe {
            STATE_REVISION = 12;
        }
        adopt_action_epoch(4);
        assert_eq!(unsafe { STATE_REVISION }, 12, "ten sam epoch => rewizja zachowana");
        // Inny epoch (reuzyta instancja) => rewizja startuje per-epoch od 0.
        adopt_action_epoch(8);
        assert_eq!(unsafe { STATE_REVISION }, 0, "nowy epoch => rewizja per-epoch od 0");
    }

    #[test]
    fn set_session_user_trims_and_treats_blank_as_anon() {
        set_panel_epoch(3);
        // Biale znaki sa trimowane.
        set_session_user(Some("  user-X  "));
        assert_eq!(session_key("k"), "f:user-X:3:k");
        // Pusty / czysto bialy user => anon.
        set_session_user(Some("   "));
        assert!(session_key("k").starts_with("f:anon:3:"));
    }

    #[test]
    fn known_fields_cover_form_allowlist_for_gc() {
        // KNOWN_FIELDS (zrodlo GC + hydratacji) musi zawierac wszystkie pola, ktore
        // `set-field` moze zapisac (is_known_field), inaczej GC zostawialby sieroty.
        for f in KNOWN_FIELDS {
            // selected_collection* nie sa polami set-field, ale sa per-sesja i tez GC-owane.
            let _ = f;
        }
        for f in [
            SP_NEW_COLLECTION,
            SP_CHAT_QUESTION,
            SP_CHAT_COLLECTION,
            SP_GRAPH_QUERY,
            SP_CONFLICT_STATUS,
        ] {
            assert!(
                KNOWN_FIELDS.contains(&f),
                "pole set-field {f} musi byc GC-owane (brak w KNOWN_FIELDS)"
            );
        }
    }

    #[test]
    fn conflict_status_options_cover_lifecycle() {
        let opts = conflict_status_options();
        let values: Vec<&str> = opts
            .iter()
            .filter_map(|o| match &o.value {
                SelectValue::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        for expected in ["open", "resolving", "escalated", "resolved_auto", "resolved_merge_pending", "all"] {
            assert!(values.contains(&expected), "brak statusu {expected}");
        }
    }
}
