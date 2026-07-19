// =============================================================================
// Plik: addons/rag/src/ui.rs
// Opis: Pelne GUI addona RAG (MemGraphRAG) przez binarny protokol CBOR (sdk-runtime).
//       Panel `main` jako Split (sidebar baz wiedzy | workspace czat-first). Sidebar
//       to klikalne karty kolekcji; workspace to naglowek bazy + NavTabs (Czat,
//       Dokumenty, Graf, Konflikty). Komponenty emitowane DEKLARATYWNIE typami SDK
//       (tentaflow_sdk_spec::protocol::ui::*); zero HTML/JS. Akcje UI (Handler ->
//       action_id) wracaja do hosta jako tool "ui.main.<action>", a host wola
//       crate::ui::handle_ui_action, ktory uderza w read/write tooly z lib.rs i
//       odswieza panel przez SlotContent / StatePatch. Sidebar i workspace to dwa
//       osobne sloty (Split renderuje puste data-slot-id, tresc kazdego idzie
//       osobnym SlotContent). Historia czatu zyje w KV (`chat_log:{collection_id}`),
//       bo addon nie czyta stanu panelu hosta — dymki buduje z KV przy renderze.
// =============================================================================

use serde_json::{json, Value as JsonValue};

use tentaflow_sdk_spec::protocol::control::CborMap;
use tentaflow_sdk_spec::protocol::ui::a11y::{Accessibility, EventKind};
use tentaflow_sdk_spec::protocol::ui::actions::Button;
use tentaflow_sdk_spec::protocol::ui::bind::{BindRef, PathSegment, StatePath};
use tentaflow_sdk_spec::protocol::ui::component::{Component, HandlerMap};
use tentaflow_sdk_spec::protocol::ui::data::{Avatar, Badge, Heading, Markdown, Table, Tag, Text};
use tentaflow_sdk_spec::protocol::ui::form::{FileInput, Input, Select, Textarea};
use tentaflow_sdk_spec::protocol::ui::handler::{FailurePolicy, Handler};
use tentaflow_sdk_spec::protocol::ui::icon_name::IconName;
use tentaflow_sdk_spec::protocol::ui::inline::{
    AvatarRef, BorderToken, DimensionToken, IconRef, NavTab, SelectOption, SelectValue, SplitSize,
    TableColumn, TableColumnWidth,
};
use tentaflow_sdk_spec::protocol::ui::layout::{
    Box, Card, Cluster, Divider, NavTabs, ScrollContainer, SectionCard, Split, Stack,
};
use tentaflow_sdk_spec::protocol::ui::molecules::EmptyState;
use tentaflow_sdk_spec::protocol::ui::panel::PanelShell;
use tentaflow_sdk_spec::protocol::ui::patch::{PatchOp, PatchOpKind};
use tentaflow_sdk_spec::protocol::ui::slot::{
    CachePolicy, SlotDecl, SlotDefault, SlotSemantics, SlotVisibility, StateEntry,
};
use tentaflow_sdk_spec::protocol::ui::slot_msg::SlotContent;
use tentaflow_sdk_spec::protocol::ui::state::StatePatch;
use tentaflow_sdk_spec::protocol::ui::tokens::{
    AvatarShape, AvatarSize, BackgroundToken, BadgeVariant, ButtonSize, ButtonVariant, CardVariant,
    ColumnRender, Density, DividerOrientation, DividerVariant, EmptyStateVariant, FlexAlign,
    FlexJustify, InputSize, InputType, LinkTarget, MarkdownFeature, NavTabsVariant, RadiusToken,
    ScrollOrientation, ShadowToken, Spacing, TableSelectMode, TableVariant, TagSize, TextAlign,
    TextStyle, Tone,
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
/// Slot sidebara (lista baz wiedzy) Splitu shellu.
const SLOT_SIDEBAR: &str = "sidebar";
/// Slot workspace (naglowek bazy + zakladki) Splitu shellu.
const SLOT_WORKSPACE: &str = "workspace";
pub const DEFAULT_TAB: &str = "chat";

// Zakladki workspace (NavTabs). Kolekcje nie ma juz zakladki — ich role pelni sidebar.
const TAB_DOCUMENTS: &str = "documents";
const TAB_CHAT: &str = "chat";
const TAB_GRAPH: &str = "graph";
const TAB_CONFLICTS: &str = "conflicts";

// Sciezki stanu panelu (StatePath::Key). Tabele czytaja wiersze z *_rows; pola
// formularzy bind-uja sie do *_input; wybrana kolekcja steruje workspace.
const SP_ACTIVE_TAB: &str = "active_tab";
const SP_NEW_COLLECTION: &str = "new_collection_name";
const SP_SIDEBAR_SEARCH: &str = "sidebar_search";
const SP_CREATE_OPEN: &str = "create_open";
const SP_SELECTED_COLLECTION: &str = "selected_collection";
const SP_SELECTED_COLLECTION_NAME: &str = "selected_collection_name";
const SP_WS_DOCCOUNT: &str = "ws_doc_count";
const SP_DOCUMENT_ROWS: &str = "document_rows";
const SP_INGEST_SUMMARY: &str = "ingest_summary";
const SP_CHAT_INPUT: &str = "chat_input";
const SP_CHAT_MESSAGES: &str = "chat_messages";
const SP_GRAPH_QUERY: &str = "graph_query";
const SP_GRAPH_CENTER: &str = "graph_center";
const SP_NEIGHBOR_ROWS: &str = "neighbor_rows";
const SP_FACT_ROWS: &str = "fact_rows";
const SP_CONFLICT_STATUS: &str = "conflict_status_filter";
const SP_CONFLICT_ROWS: &str = "conflict_rows";
const SP_CONFLICT_DETAIL: &str = "conflict_detail_text";
const SP_STATUS_MESSAGE: &str = "status_message";
/// Sciezka stanu panelu wiazaca Select przelacznika "Baza grafowa" w zakladce Kolekcje.
/// Wartosc "1"/"0" (string) odzwierciedla aktualny config instancji (graph_enabled).
const SP_GRAPH_ENABLED: &str = "graph_enabled_toggle";

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
    SP_SIDEBAR_SEARCH,
    SP_SELECTED_COLLECTION,
    SP_SELECTED_COLLECTION_NAME,
    SP_CHAT_INPUT,
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

/// Nadaje komponentowi dostepna nazwe (ARIA label) przez pole `a11y`. Renderer
/// wymaga jej dla pol formularza bez widocznego `label` (inaczej `tf-input`/`tf-select`
/// odrzucaja caly SlotContent z bledem "without 'label' field requires a11y.label").
fn with_a11y_label(mut c: Component, label: &str) -> Component {
    c.a11y = Some(Accessibility {
        label: Some(lit(label)),
        ..Accessibility::default()
    });
    c
}

// =============================================================================
// PanelShell — NavTabs (5 zakladek) + host slotu tresci
// =============================================================================

fn nav_tab(id: &str, label: &str) -> NavTab {
    nav_tab_locked(id, label, false)
}

/// Zakladka NavTabs z opcjonalnym zamkiem. Graf i Konflikty sa `locked` gdy warstwa
/// grafu jest wylaczona (czysty RAG wektorowy nie ma grafu ani konfliktow do pokazania)
/// — uzytkownik widzi, ze funkcja istnieje, ale jest niedostepna do czasu zalaczenia
/// grafu w zakladce Kolekcje.
fn nav_tab_locked(id: &str, label: &str, locked: bool) -> NavTab {
    NavTab {
        id: id.into(),
        label: lit(label),
        icon: None,
        badge: None,
        panel_id: None,
        locked,
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
    // Split: 22% sidebar (baz wiedzy) | 78% workspace (czat-first). Renderer zostawia
    // dwa puste data-slot-id; tresc kazdego panelu idzie osobnym SlotContent ponizej.
    let layout = Split {
        orientation: tentaflow_sdk_spec::protocol::ui::tokens::SplitOrientation::Horizontal,
        primary_size: SplitSize::Percent { value: 22.0 },
        min_primary: 220,
        max_primary: 420,
        resizable: true,
        primary_slot: SLOT_SIDEBAR.into(),
        secondary_slot: SLOT_WORKSPACE.into(),
        collapse_below: None,
        divider: None,
        grow: None,
    }
    .into_component("root")
    .expect("kodowanie Split root");

    let shell = PanelShell {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: panel_epoch(),
        layout,
        slots: vec![
            SlotDecl {
                id: SLOT_SIDEBAR.into(),
                semantics: SlotSemantics::SidePanel,
                default_state: SlotDefault::Loading,
                cache_policy: CachePolicy::None,
                visibility: SlotVisibility::Always,
                max_payload_bytes: None,
            },
            SlotDecl {
                id: SLOT_WORKSPACE.into(),
                semantics: SlotSemantics::MainContent,
                default_state: SlotDefault::Loading,
                cache_policy: CachePolicy::None,
                visibility: SlotVisibility::Always,
                max_payload_bytes: None,
            },
        ],
        initial_state: initial_state_entries(),
        initial_commands: vec![],
    };

    send_ui(&UiPayload::PanelShell(shell));

    // Split renderuje puste sloty — wypchnij tresc obu osobnym SlotContent.
    send_sidebar();
    send_workspace(&active_tab());
}

/// Stan poczatkowy panelu — wszystkie sciezki, ktorych dotykaja bind-y/tabele,
/// musza istniec od startu (inaczej tabele renderuja sie puste, a inputy bez wartosci).
fn initial_state_entries() -> Vec<StateEntry> {
    let empty_arr = || CborValue::Array(vec![]);
    let empty_str = || CborValue::Text("".into());
    vec![
        StateEntry { path: state_path(SP_ACTIVE_TAB), value: CborValue::Text(DEFAULT_TAB.into()) },
        StateEntry { path: state_path(SP_NEW_COLLECTION), value: empty_str() },
        StateEntry { path: state_path(SP_SIDEBAR_SEARCH), value: empty_str() },
        StateEntry { path: state_path(SP_CREATE_OPEN), value: CborValue::Text("0".into()) },
        StateEntry { path: state_path(SP_SELECTED_COLLECTION), value: empty_str() },
        StateEntry { path: state_path(SP_SELECTED_COLLECTION_NAME), value: empty_str() },
        StateEntry { path: state_path(SP_WS_DOCCOUNT), value: empty_str() },
        StateEntry { path: state_path(SP_DOCUMENT_ROWS), value: empty_arr() },
        StateEntry { path: state_path(SP_INGEST_SUMMARY), value: empty_str() },
        StateEntry { path: state_path(SP_CHAT_INPUT), value: empty_str() },
        StateEntry { path: state_path(SP_CHAT_MESSAGES), value: empty_arr() },
        StateEntry { path: state_path(SP_GRAPH_QUERY), value: empty_str() },
        StateEntry { path: state_path(SP_GRAPH_CENTER), value: empty_str() },
        StateEntry { path: state_path(SP_NEIGHBOR_ROWS), value: empty_arr() },
        StateEntry { path: state_path(SP_FACT_ROWS), value: empty_arr() },
        StateEntry { path: state_path(SP_CONFLICT_STATUS), value: CborValue::Text("open".into()) },
        StateEntry { path: state_path(SP_CONFLICT_ROWS), value: empty_arr() },
        StateEntry { path: state_path(SP_CONFLICT_DETAIL), value: empty_str() },
        StateEntry { path: state_path(SP_STATUS_MESSAGE), value: empty_str() },
        StateEntry {
            path: state_path(SP_GRAPH_ENABLED),
            value: CborValue::Text(if ui_graph_enabled() { "1" } else { "0" }.into()),
        },
    ]
}

/// Aktualna zakladka workspace z KV sesji (domyslnie czat). Sterowanie nawigacja
/// trzymamy w polu sesyjnym, by re-push workspace po akcjach trafial w wlasciwy widok.
fn active_tab() -> String {
    field_value(SP_ACTIVE_TAB).unwrap_or_else(|| DEFAULT_TAB.to_string())
}

// =============================================================================
// SlotContent — sidebar + workspace (dwa osobne sloty Splitu)
// =============================================================================

/// Wypycha tresc sidebara (lista baz wiedzy). Overlay niesie pola sterowane bindami
/// (search + flaga inline-create), zeby input zachowal wartosc po re-renderze.
pub fn send_sidebar() {
    let overlay = vec![
        StateEntry {
            path: state_path(SP_SIDEBAR_SEARCH),
            value: CborValue::Text(field_value(SP_SIDEBAR_SEARCH).unwrap_or_default()),
        },
        StateEntry {
            path: state_path(SP_CREATE_OPEN),
            value: CborValue::Text(if create_open() { "1" } else { "0" }.into()),
        },
        StateEntry {
            path: state_path(SP_NEW_COLLECTION),
            value: CborValue::Text(field_value(SP_NEW_COLLECTION).unwrap_or_default()),
        },
    ];
    send_ui(&UiPayload::SlotContent(SlotContent {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: panel_epoch(),
        slot_id: SLOT_SIDEBAR.into(),
        fragment: sidebar_view(),
        state_overlay: Some(overlay),
    }));
}

/// Wypycha tresc workspace dla zadanej zakladki. Utrwala aktywna zakladke w KV sesji
/// (zrodlo prawdy dla re-pushy) i dolacza overlay danych/pol tej zakladki.
pub fn send_workspace(tab: &str) {
    set_kv(SP_ACTIVE_TAB, tab);
    let mut overlay = vec![StateEntry {
        path: state_path(SP_ACTIVE_TAB),
        value: CborValue::Text(tab.into()),
    }];
    overlay.extend(workspace_data_overlay(tab));
    overlay.extend(workspace_field_overlay(tab));

    send_ui(&UiPayload::SlotContent(SlotContent {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: panel_epoch(),
        slot_id: SLOT_WORKSPACE.into(),
        fragment: workspace_view(tab),
        state_overlay: Some(overlay),
    }));
}

/// Dane (wiersze tabel) ladowane razem z fragmentem workspace — tabele czytaja je
/// ze sciezek stanu (rows_path), wiec overlay musi je dostarczyc przy renderze.
fn workspace_data_overlay(tab: &str) -> Vec<StateEntry> {
    let collection = selected_collection();
    if collection.is_empty() {
        return vec![];
    }
    match tab {
        TAB_DOCUMENTS => vec![
            StateEntry { path: state_path(SP_DOCUMENT_ROWS), value: load_document_rows(&collection) },
            StateEntry {
                path: state_path(SP_INGEST_SUMMARY),
                value: CborValue::Text(load_ingest_summary(&collection)),
            },
        ],
        TAB_CONFLICTS => vec![StateEntry {
            path: state_path(SP_CONFLICT_ROWS),
            value: load_conflict_rows(&conflict_status_filter()),
        }],
        _ => vec![],
    }
}

/// HYDRATACJA pol formularza workspace: wstawia aktualna per-sesyjna wartosc pola (z
/// KV) do renderowanej sciezki stanu, zeby UI pokazywal to samo co backend. Naglowek
/// workspace (nazwa bazy + licznik dokumentow + przelacznik grafu) tez jest sterowany.
fn workspace_field_overlay(tab: &str) -> Vec<StateEntry> {
    let hydrate = |field: &str, default: &str| StateEntry {
        path: state_path(field),
        value: CborValue::Text(field_value(field).unwrap_or_else(|| default.to_string())),
    };
    let mut entries = vec![
        hydrate(SP_SELECTED_COLLECTION_NAME, ""),
        StateEntry {
            path: state_path(SP_WS_DOCCOUNT),
            value: CborValue::Text(field_value(SP_WS_DOCCOUNT).unwrap_or_default()),
        },
        StateEntry {
            path: state_path(SP_GRAPH_ENABLED),
            value: CborValue::Text(if ui_graph_enabled() { "1" } else { "0" }.into()),
        },
    ];
    match tab {
        TAB_CHAT => entries.push(hydrate(SP_CHAT_INPUT, "")),
        TAB_GRAPH => entries.push(hydrate(SP_GRAPH_QUERY, "")),
        TAB_CONFLICTS => entries.push(hydrate(SP_CONFLICT_STATUS, "open")),
        _ => {}
    }
    entries
}

// =============================================================================
// Sidebar — lista baz wiedzy (klikalne karty)
// =============================================================================

/// Czy panel inline-create kolekcji jest otwarty (KV sesji "1"/"0").
fn create_open() -> bool {
    field_value(SP_CREATE_OPEN).map(|v| v == "1").unwrap_or(false)
}

/// Filtr listy baz w sidebarze (substring, lowercase). Pusty => brak filtra.
fn sidebar_filter() -> String {
    field_value(SP_SIDEBAR_SEARCH).unwrap_or_default().to_lowercase()
}

/// Widok sidebara: naglowek + przycisk "Nowa", wyszukiwarka, lista kart kolekcji.
/// Karty buduje JAWNIE (Sidebar.items sa plaskie — nie obsluguja bogatych itemow).
fn sidebar_view() -> Component {
    let title = heading("sb-title", "Bazy wiedzy");
    let mut new_btn = Button {
        variant: ButtonVariant::Ghost,
        tone: Tone::Primary,
        label: lit("Nowa"),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Sm,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Compact,
    }
    .into_component("sb-new")
    .expect("kodowanie Button new");
    new_btn.handlers = Some(HandlerMap(vec![backend_handler(
        EventKind::Click,
        "start-create-collection",
    )]));
    let header = cluster_between("sb-header", vec![title, new_btn]);

    let mut search = Input {
        r#type: InputType::Text,
        bind_path: state_path(SP_SIDEBAR_SEARCH),
        placeholder: Some(lit("Szukaj bazy…")),
        label: None,
        hint: None,
        leading_icon: None,
        trailing_icon: None,
        prefix: None,
        suffix: None,
        validators: vec![],
        max_length: Some(128),
        min_length: None,
        pattern: None,
        autocomplete: None,
        input_mode: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
        variant: None,
    }
    .into_component("sb-search")
    .expect("kodowanie Input search");
    // HandlerMap dopuszcza JEDEN handler na EventKind, wiec zapis wartosci i
    // re-render listy lacza sie w jedna akcje: `filter-collections` niesie `field`
    // i sam zapisuje value do KV przed przerenderowaniem sidebara.
    search.handlers = Some(HandlerMap(vec![(
        EventKind::Change,
        Handler::Backend {
            action_id: "filter-collections".into(),
            params: CborMap(vec![(
                "field".into(),
                CborValue::Text(SP_SIDEBAR_SEARCH.into()),
            )]),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    let search = with_a11y_label(search, "Szukaj bazy wiedzy");

    let collections = list_collections_data();
    let filter = sidebar_filter();
    let selected = selected_collection();
    let graph_on = ui_graph_enabled();
    let cards: Vec<Component> = collections
        .iter()
        .filter(|c| {
            filter.is_empty()
                || c.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&filter)
        })
        .enumerate()
        .map(|(i, c)| collection_card(i, c, &selected, graph_on))
        .collect();

    let list: Component = if cards.is_empty() {
        empty_state(
            "sb-empty",
            IconName::Database,
            "Brak baz wiedzy",
            Some("Utworz pierwsza baze przyciskiem „Nowa”."),
            EmptyStateVariant::Compact,
            None,
        )
    } else {
        ScrollContainer {
            orientation: ScrollOrientation::Vertical,
            height: DimensionToken::Full,
            max_height: None,
            children: cards,
            sticky_header_slot: None,
            virtualize: false,
            // gap:Sm wymusza display:flex z odstepem miedzy kartami — naprawia
            // stykajace sie pionowo karty kolekcji (gola lista nie ma zadnego gapu).
            gap: Some(Spacing::Sm),
        }
        .into_component("sb-scroll")
        .expect("kodowanie ScrollContainer sidebar")
    };

    let mut children = vec![header, search, list];
    if create_open() {
        children.push(sidebar_create_card());
    }
    // Sidebar zwarty: gap:Sm (nie Md) — naglowek, search i lista trzymaja sie ciasno.
    sidebar_stack("tab-sidebar", children)
}

/// Pojedyncza klikalna karta kolekcji w sidebarze: nazwa + licznik dokumentow +
/// Badge statusu grafu. Aktywna karta (id == wybrana kolekcja) dostaje akcent.
fn collection_card(index: usize, c: &JsonValue, selected: &str, graph_on: bool) -> Component {
    let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = c.get("name").and_then(|v| v.as_str()).unwrap_or(id);
    let docs = c.get("document_count").and_then(|v| v.as_i64()).unwrap_or(0);
    let active = !id.is_empty() && id == selected;

    let info = Stack {
        gap: Spacing::Xxs,
        align: FlexAlign::Start,
        children: vec![
            strong_text(&format!("cc-name-{index}"), name),
            muted_caption(&format!("cc-docs-{index}"), lit(&format!("{docs} dok"))),
        ],
        padding: None,
        justify: None,
        style: None,
        responsive: None,
    }
    .into_component(format!("cc-info-{index}"))
    .expect("kodowanie Stack card info");

    let badge = Badge {
        variant: BadgeVariant::Dot,
        tone: if graph_on { Tone::Success } else { Tone::Neutral },
        label: lit(if graph_on { "graf" } else { "wektor" }),
        icon: None,
        count: None,
        // Dot badge nie pokazuje liczby, ale renderer (data-stat-labels-renderer.js:495)
        // wymaga `max > 0` dla KAZDEGO wariantu Badge — stad sentinel 99.
        max: 99,
        pulse: false,
    }
    .into_component(format!("cc-badge-{index}"))
    .expect("kodowanie Badge card");

    // Wiersz karty: lewa kolumna info rosnie (box_grow), Badge zostaje staly po prawej
    // i pionowo wysrodkowany (align:Center). wrap:false (cluster_row_between) gwarantuje,
    // ze Badge nie spada pod nazwe ani sie nie lamie przy ciasnym sidebarze.
    let row = cluster_row_between(
        &format!("cc-row-{index}"),
        vec![box_grow(&format!("cc-info-grow-{index}"), info), badge],
    );

    // Aktywna karta: wypelnienie + akcent + lekki cien (wyrazny stan „selected").
    // Nieaktywna: tlo `Muted` (8% jasnosci) zamiast `Subtle` (4%, karty znikaly) —
    // powierzchnia musi byc wyczuwalna jako karta; hairline-ramka domyka ksztalt.
    let (variant, accent, shadow, border, background) = if active {
        (
            CardVariant::Filled,
            Some(Tone::Primary),
            ShadowToken::Subtle,
            BorderToken::Accent { tone: Tone::Primary },
            BackgroundToken::Accent,
        )
    } else {
        (
            CardVariant::Outlined,
            None,
            ShadowToken::None,
            BorderToken::Hairline,
            BackgroundToken::Muted,
        )
    };
    let mut card = Card {
        variant,
        // padding:Sm zamiast Md — karta ~48-52px (gesta lista) zamiast 75px.
        padding: Spacing::Sm,
        gap: Spacing::Xs,
        radius: RadiusToken::Md,
        shadow,
        border,
        background,
        accent,
        children: vec![row],
        interactive: true,
        clickable: true,
        style: None,
    }
    .into_component(format!("cc-{index}"))
    .expect("kodowanie Card kolekcji");
    card.handlers = Some(HandlerMap(vec![(
        EventKind::Click,
        Handler::Backend {
            action_id: "open-collection".into(),
            params: CborMap(vec![("id".into(), CborValue::Text(id.into()))]),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    card
}

/// Inline-karta tworzenia kolekcji na dole sidebara (gdy SP_CREATE_OPEN == "1").
fn sidebar_create_card() -> Component {
    let name = text_input("sb-new-name", SP_NEW_COLLECTION, "", "Nazwa nowej bazy");
    let create = action_button(
        "sb-create-ok",
        "Utworz",
        "create-collection",
        ButtonVariant::Primary,
        Tone::Primary,
    );
    let cancel = action_button(
        "sb-create-cancel",
        "Anuluj",
        "cancel-create-collection",
        ButtonVariant::Ghost,
        Tone::Neutral,
    );
    let actions = Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: vec![create, cancel],
        wrap: Some(false),
    }
    .into_component("sb-create-actions")
    .expect("kodowanie Cluster create");

    Card {
        variant: CardVariant::Outlined,
        padding: Spacing::Sm,
        gap: Spacing::Sm,
        radius: RadiusToken::Md,
        shadow: ShadowToken::None,
        border: BorderToken::Thin,
        background: BackgroundToken::Subtle,
        accent: None,
        children: vec![name, actions],
        interactive: false,
        clickable: false,
        style: None,
    }
    .into_component("sb-create")
    .expect("kodowanie Card create")
}

// =============================================================================
// Workspace — naglowek bazy + NavTabs + widok aktywnej zakladki
// =============================================================================

/// Widok workspace. Bez wybranej bazy => EmptyState zachecajacy do wyboru/utworzenia.
/// Z baza => naglowek (nazwa + licznik + przelacznik grafu + usun) + NavTabs + tresc.
fn workspace_view(tab: &str) -> Component {
    if selected_collection().is_empty() {
        let new_btn = action_button(
            "ws-empty-new",
            "Nowa baza wiedzy",
            "start-create-collection",
            ButtonVariant::Primary,
            Tone::Primary,
        );
        // Default (nie Illustrated) — mniej pustej przestrzeni, zwarty stos
        // ikona+tytul+podtytul+CTA wysrodkowany przez sam komponent EmptyState.
        return empty_state(
            "ws-empty",
            IconName::Database,
            "Wybierz baze wiedzy",
            Some("Wybierz baze z listy po lewej albo utworz nowa, aby zaczac rozmowe."),
            EmptyStateVariant::Default,
            Some(new_btn),
        );
    }

    let header = workspace_header();
    let divider = Divider {
        orientation: DividerOrientation::Horizontal,
        variant: DividerVariant::Subtle,
        spacing: Spacing::Sm,
        label: None,
    }
    .into_component("ws-divider")
    .expect("kodowanie Divider");
    let nav = workspace_nav(tab);
    let content = match tab {
        TAB_DOCUMENTS => documents_tab(),
        TAB_GRAPH => graph_tab(),
        TAB_CONFLICTS => conflicts_tab(),
        _ => chat_view(),
    };

    stack("tab-workspace", vec![header, divider, nav, content])
}

/// Naglowek workspace: nazwa bazy + Tag z liczba dokumentow + Select grafu + Usun.
fn workspace_header() -> Component {
    // Heading bind do nazwy wybranej bazy (czysta nazwa, bez prefiksu "Kolekcja:").
    let name = bound_heading("ws-name", SP_SELECTED_COLLECTION_NAME);

    let doc_tag = Tag {
        tone: Tone::Info,
        label: bound(SP_WS_DOCCOUNT),
        size: TagSize::Sm,
    }
    .into_component("ws-doctag")
    .expect("kodowanie Tag doccount");

    let mut graph_toggle = Select {
        bind_path: state_path(SP_GRAPH_ENABLED),
        options: graph_enabled_options(),
        placeholder: None,
        label: None,
        searchable: false,
        clearable: false,
        virtualize: false,
        disabled: None,
        size: InputSize::Sm,
        groups: None,
    }
    .into_component("ws-graph-toggle")
    .expect("kodowanie Select graph");
    graph_toggle.handlers = Some(HandlerMap(vec![backend_handler(
        EventKind::Change,
        "set-graph-enabled",
    )]));
    let graph_toggle = with_a11y_label(graph_toggle, "Baza grafowa");

    let delete = action_button(
        "ws-delete",
        "Usun baze",
        "delete-collection",
        ButtonVariant::Ghost,
        Tone::Critical,
    );

    let left = Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: vec![name, doc_tag],
        wrap: Some(false),
    }
    .into_component("ws-header-left")
    .expect("kodowanie Cluster header left");
    let right = Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::End,
        children: vec![graph_toggle, delete],
        wrap: Some(false),
    }
    .into_component("ws-header-right")
    .expect("kodowanie Cluster header right");

    cluster_between("ws-header", vec![left, right])
}

/// NavTabs workspace. Graf/Konflikty sa locked, gdy warstwa grafu jest wylaczona.
fn workspace_nav(_tab: &str) -> Component {
    let graph_on = ui_graph_enabled();
    let mut nav = NavTabs {
        items: vec![
            nav_tab(TAB_CHAT, "Czat"),
            nav_tab(TAB_DOCUMENTS, "Dokumenty"),
            nav_tab_locked(TAB_GRAPH, "Graf", !graph_on),
            nav_tab_locked(TAB_CONFLICTS, "Konflikty", !graph_on),
        ],
        active_id: bound(SP_ACTIVE_TAB),
        variant: NavTabsVariant::Underlined,
        scroll_overflow: true,
    }
    .into_component("ws-nav")
    .expect("kodowanie NavTabs workspace");
    nav.handlers = Some(HandlerMap(vec![backend_handler(
        EventKind::Select,
        "panel-navigate",
    )]));
    nav
}

// =============================================================================
// Czat — dymki z historii (KV) + pasek wejscia
// =============================================================================

/// Widok czatu: przewijalna lista dymkow z historii KV + pinowany pasek wejscia.
fn chat_view() -> Component {
    let collection = selected_collection();
    let log = load_chat_log(&collection);

    let bubbles: Vec<Component> = log
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if role.is_empty() {
                return None;
            }
            Some(message_bubble(i, role, content))
        })
        .collect();

    let scroll: Component = if bubbles.is_empty() {
        empty_state(
            "chat-empty",
            IconName::Chat,
            "Zadaj pierwsze pytanie",
            Some("Np. „Podsumuj kluczowe wnioski z dokumentow w tej bazie.”"),
            EmptyStateVariant::Default,
            None,
        )
    } else {
        ScrollContainer {
            orientation: ScrollOrientation::Vertical,
            height: DimensionToken::Full,
            max_height: None,
            children: bubbles,
            sticky_header_slot: None,
            virtualize: false,
            // Rowny rytm miedzy dymkami rozmowy (md = wyrazne rozdzielenie tur).
            gap: Some(Spacing::Md),
        }
        .into_component("chat-scroll")
        .expect("kodowanie ScrollContainer chat")
    };

    let mut input = Textarea {
        bind_path: state_path(SP_CHAT_INPUT),
        placeholder: Some(lit("Zadaj pytanie…")),
        label: None,
        hint: None,
        validators: vec![],
        max_length: Some(4096),
        min_length: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
        rows: 2,
        autoresize: true,
        max_rows: Some(8),
        monospace: false,
        variant: None,
    }
    .into_component("chat-input")
    .expect("kodowanie Textarea chat");
    input.handlers = Some(HandlerMap(vec![
        set_field_handler(EventKind::Change, SP_CHAT_INPUT),
        set_field_handler(EventKind::Submit, SP_CHAT_INPUT),
    ]));
    let input = with_a11y_label(input, "Tresc pytania");

    let send = action_button(
        "chat-send",
        "Wyslij",
        "ask-question",
        ButtonVariant::Primary,
        Tone::Primary,
    );
    let input_bar = Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::End,
        justify: FlexJustify::Start,
        children: vec![box_grow("chat-input-grow", input), send],
        // Pole tekstowe rosnie (box_grow), przycisk „Wyslij" zostaje staly po prawej;
        // bez zawijania, by pasek wejscia byl jednym spojnym rzedem przyklejonym na dole.
        wrap: Some(false),
    }
    .into_component("chat-input-bar")
    .expect("kodowanie Cluster input bar");

    // Czat: lista dymkow rosnie (box_grow), pasek wejscia przyklejony na dole.
    // justify:SpaceBetween rozpycha scroll i pasek na pelna wysokosc zakladki.
    Stack {
        gap: Spacing::Md,
        align: FlexAlign::Stretch,
        children: vec![box_grow("chat-scroll-grow", scroll), input_bar],
        padding: Some(Spacing::Md),
        justify: Some(FlexJustify::SpaceBetween),
        style: None,
        responsive: None,
    }
    .into_component("tab-chat")
    .expect("kodowanie Stack chat")
}

/// Dymek wiadomosci. user => karta wyrownana do prawej z akcentem; assistant =>
/// avatar + karta z Markdownem wyrownana do lewej.
fn message_bubble(index: usize, role: &str, content: &str) -> Component {
    if role == "user" {
        let card = Card {
            variant: CardVariant::Filled,
            padding: Spacing::Sm,
            gap: Spacing::Xs,
            radius: RadiusToken::Lg,
            shadow: ShadowToken::None,
            border: BorderToken::None,
            background: BackgroundToken::Accent,
            accent: Some(Tone::Primary),
            children: vec![body_text(&format!("msg-u-{index}"), lit(content))],
            interactive: false,
            clickable: false,
            style: None,
        }
        .into_component(format!("msg-card-{index}"))
        .expect("kodowanie Card user");
        Cluster {
            gap: Spacing::Sm,
            align: FlexAlign::Start,
            justify: FlexJustify::End,
            children: vec![card],
            wrap: Some(false),
        }
        .into_component(format!("msg-{index}"))
        .expect("kodowanie Cluster user bubble")
    } else {
        let avatar = Avatar {
            source: AvatarRef::Initials { initials: "AI".into() },
            size: AvatarSize::Sm,
            shape: AvatarShape::Circle,
            status: None,
            tone: Some(Tone::Primary),
        }
        .into_component(format!("msg-av-{index}"))
        .expect("kodowanie Avatar");
        let card = Card {
            variant: CardVariant::Filled,
            padding: Spacing::Sm,
            gap: Spacing::Xs,
            radius: RadiusToken::Lg,
            shadow: ShadowToken::None,
            border: BorderToken::None,
            // Muted (8%) zamiast Subtle (4%) — dymek asystenta ma byc wyrazna
            // powierzchnia odrozniona od tla panelu, podobnie jak karty kolekcji.
            background: BackgroundToken::Muted,
            accent: None,
            children: vec![chat_markdown(&format!("msg-md-{index}"), content)],
            interactive: false,
            clickable: false,
            style: None,
        }
        .into_component(format!("msg-card-{index}"))
        .expect("kodowanie Card assistant");
        Cluster {
            gap: Spacing::Sm,
            align: FlexAlign::Start,
            justify: FlexJustify::Start,
            // Avatar staly, dymek rosnie — bez zawijania, by avatar nie odskakiwal pod tresc.
            children: vec![avatar, box_grow(&format!("msg-grow-{index}"), card)],
            wrap: Some(false),
        }
        .into_component(format!("msg-{index}"))
        .expect("kodowanie Cluster assistant bubble")
    }
}

/// Markdown odpowiedzi asystenta (literal — tresc juz wbudowana w dymek z KV).
fn chat_markdown(id: &str, content: &str) -> Component {
    Markdown {
        content: lit(content),
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
    .into_component(id)
    .expect("kodowanie Markdown chat")
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

/// Naglowek z trescia bind-owana do sciezki stanu (nazwa wybranej bazy).
fn bound_heading(id: &str, key: &str) -> Component {
    Heading {
        content: bound(key),
        level: 3,
        tone: None,
        align: None,
    }
    .into_component(id)
    .expect("kodowanie Heading bound")
}

/// Cluster z trescia rozsunieta na krawedzie (space-between) — naglowki sidebara/workspace.
fn cluster_between(id: &str, children: Vec<Component>) -> Component {
    Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::SpaceBetween,
        children,
        // Naglowki sidebara/workspace nie moga sie zawijac — tytul i akcje zostaja
        // w jednym rzedzie nawet przy ciasnej szerokosci.
        wrap: Some(false),
    }
    .into_component(id)
    .expect("kodowanie Cluster between")
}

/// Cluster jednorzedowy (bez zawijania): info rosnie po lewej, element staly po prawej.
/// Uzywany w karcie kolekcji, zeby Badge nie spadal pod nazwe ani sie nie lamie.
fn cluster_row_between(id: &str, children: Vec<Component>) -> Component {
    Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::SpaceBetween,
        children,
        wrap: Some(false),
    }
    .into_component(id)
    .expect("kodowanie Cluster row")
}

/// Box rosnacy (flex-grow) wokol dziecka — w wierszu „info ⟷ badge" pozwala kolumnie
/// informacji zajac cala wolna szerokosc, a element po prawej zostaje staly.
fn box_grow(id: &str, child: Component) -> Component {
    Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: vec![child],
        style: None,
        direction: None,
        gap: None,
        align: None,
        justify: None,
        responsive: None,
    }
    .into_component(id)
    .expect("kodowanie Box grow")
}

/// SectionCard grupujacy sekcje workspace: spojny padding/gap, naglowek + opcjonalny
/// podtytul/akcje, jednolita ramka. Daje sekcjom „oddech" bez recznych Spacerow.
fn section_card(
    id: &str,
    title: &str,
    subtitle: Option<&str>,
    header_actions: Vec<Component>,
    body: Vec<Component>,
) -> Component {
    SectionCard {
        title: lit(title),
        subtitle: subtitle.map(lit),
        header_actions,
        header_divider: true,
        body,
        footer: None,
        padding: Spacing::Md,
        gap: Spacing::Md,
        variant: CardVariant::Outlined,
        radius: RadiusToken::Lg,
        shadow: ShadowToken::Subtle,
        border: BorderToken::Hairline,
        background: BackgroundToken::None,
        accent: None,
        style: None,
    }
    .into_component(id)
    .expect("kodowanie SectionCard")
}

/// EmptyState z opcjonalnym przyciskiem akcji glownej. Ikona nazwana (sprite SDK).
fn empty_state(
    id: &str,
    icon: IconName,
    heading_text: &str,
    message: Option<&str>,
    variant: EmptyStateVariant,
    primary: Option<Component>,
) -> Component {
    EmptyState {
        icon: IconRef::Named { name: icon, size: None, tone: None },
        heading: lit(heading_text),
        message: message.map(lit),
        primary_action: primary,
        secondary_action: None,
        variant,
    }
    .into_component(id)
    .expect("kodowanie EmptyState")
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
        streaming: None,
    }
    .into_component(id)
    .expect("kodowanie Text")
}

/// Pogrubiony tekst (np. nazwa kolekcji w karcie) — wyrazna hierarchia wzgledem
/// sekundarnego podpisu „N dok".
fn strong_text(id: &str, content: &str) -> Component {
    Text {
        content: lit(content),
        style: TextStyle::BodyStrong,
        tone: None,
        align: None,
        wrap: None,
        max_lines: Some(1),
        format: None,
        streaming: None,
    }
    .into_component(id)
    .expect("kodowanie Text strong")
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
        streaming: None,
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
        justify: None,
        style: None,
        responsive: None,
    }
    .into_component(id)
    .expect("kodowanie Stack")
}

/// Zwarty stos sidebara: gap:Sm i padding:Sm — sidebar ma byc gesty (naglowek,
/// search, lista kart blisko siebie), inaczej niz luzny workspace (`stack` gap:Md).
fn sidebar_stack(id: &str, children: Vec<Component>) -> Component {
    Stack {
        gap: Spacing::Sm,
        align: FlexAlign::Stretch,
        children,
        padding: Some(Spacing::Sm),
        justify: None,
        style: None,
        responsive: None,
    }
    .into_component(id)
    .expect("kodowanie Stack sidebar")
}

/// Pionowy stos sekcji workspace bez wlasnego paddingu — sekcje (SectionCard) maja
/// juz swoj padding, wiec zewnetrzny stos daje tylko rytm `md` miedzy nimi.
fn section_stack(id: &str, children: Vec<Component>) -> Component {
    Stack {
        gap: Spacing::Md,
        align: FlexAlign::Stretch,
        children,
        padding: Some(Spacing::Md),
        justify: None,
        style: None,
        responsive: None,
    }
    .into_component(id)
    .expect("kodowanie Stack sekcji")
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
        variant: None,
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
// Zakladka — Dokumenty + status ingestu
// =============================================================================

fn documents_tab() -> Component {
    let summary = muted_caption("doc-ingest-summary", bound(SP_INGEST_SUMMARY));

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
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into(),
        ],
        max_size_bytes: 64 * 1024 * 1024,
        // 64: pozwala wgrac caly korpus jednym wyborem (jedna akcja = zero
        // nadpisywania pending uploadow; przy 10 batchowanie z GUI gubilo pliki).
        max_files: 64,
        multiple: true,
        drag_and_drop: true,
        capture: None,
        upload_action_id: "ingest-uploaded".into(),
        label: Some(lit("Wgraj dokumenty (PDF / obrazy / xlsx / docx / tekst)")),
        hint: Some(lit(
            "Po wgraniu uruchamiany jest pelny ingest (PDF / obrazy / xlsx / docx / tekst): parse -> chunk -> embedding.",
        )),
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

    // Sekcja „Wgraj dokumenty": dropzone + podsumowanie statusu ingestu.
    let ingest_section = section_card(
        "doc-ingest-section",
        "Wgraj dokumenty",
        Some("Po wgraniu uruchamiany jest pelny ingest: parse -> chunk -> embedding."),
        vec![],
        vec![upload, summary, muted_caption("doc-status", bound(SP_STATUS_MESSAGE))],
    );

    // Sekcja „Dokumenty": akcja odswiezenia w naglowku, tabela z oddechem ponizej.
    let list_section = section_card(
        "doc-list-section",
        "Dokumenty",
        None,
        vec![refresh],
        vec![tbl],
    );

    section_stack("tab-documents", vec![ingest_section, list_section])
}

// =============================================================================
// Zakladka — Graf (explorer)
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

    // Sekcja eksploracji: pole encji + przycisk w jednym rzedzie (pole rosnie),
    // ponizej nazwa biezacej encji i status.
    let explore_row = Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::End,
        justify: FlexJustify::Start,
        children: vec![box_grow("graph-query-grow", query), explore],
        wrap: Some(false),
    }
    .into_component("graph-explore-row")
    .expect("kodowanie Cluster graph explore");

    let explore_section = section_card(
        "graph-explore-section",
        "Graf wiedzy",
        Some("Eksploruj encje i ich powiazania w bazie."),
        vec![],
        vec![explore_row, center, muted_caption("graph-status", bound(SP_STATUS_MESSAGE))],
    );

    let neighbors_section = section_card("graph-neighbors-section", "Sasiedztwo", None, vec![], vec![neighbors]);
    let facts_section = section_card("graph-facts-section", "Fakty", None, vec![], vec![facts]);

    section_stack("tab-graph", vec![explore_section, neighbors_section, facts_section])
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
    let admin_row = Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: vec![scan, resolve, merge],
        wrap: Some(true),
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

    // Pasek filtra: select statusu (rosnie) + przycisk „Filtruj".
    let filter_row = Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::End,
        justify: FlexJustify::Start,
        children: vec![box_grow("conf-status-grow", status_select), refresh],
        wrap: Some(false),
    }
    .into_component("conf-filter-row")
    .expect("kodowanie Cluster conf filter");

    // Sekcja sterowania: filtr + reczne wyzwalacze agentow + biezacy status.
    let controls_section = section_card(
        "conf-controls-section",
        "Konflikty",
        Some("Filtruj liste i recznie wyzwalaj agentow detekcji/adjudykacji."),
        vec![],
        vec![filter_row, admin_row, muted_caption("conf-status-msg", bound(SP_STATUS_MESSAGE))],
    );

    let list_section = section_card("conf-list-section", "Lista konfliktow", None, vec![], vec![tbl]);
    let detail_section = section_card("conf-detail-section", "Szczegoly konfliktu", None, vec![], vec![detail]);

    section_stack("tab-conflicts", vec![controls_section, list_section, detail_section])
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

/// Czy warstwa grafu (MemGraphRAG) jest wlaczona dla tej instancji. Czyta TEN SAM
/// instancyjny klucz KV, ktorego uzywa `lib.rs::graph_enabled()` (config instancji, nie
/// pole sesji — wiec BEZ session_key). UI bramkuje nim widocznosc zakladek Graf/Konflikty
/// i stan przelacznika; backend (`graph_enabled()`) pozostaje jedynym zrodlem prawdy dla
/// logiki ingest/query/tools.
fn ui_graph_enabled() -> bool {
    crate::state_get(crate::GRAPH_ENABLED_STATE_KEY)
        .ok()
        .flatten()
        .map(|b| {
            let s = String::from_utf8_lossy(&b);
            let s = s.trim();
            s.eq_ignore_ascii_case("1") || s.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
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

/// Surowa lista kolekcji (id, name, document_count, created_at) z read-toola.
/// Sidebar buduje z niej jawne karty (filtr + akcent aktywnej).
fn list_collections_data() -> Vec<JsonValue> {
    let res = crate::handle_list_collections();
    res.get("data")
        .and_then(|d| d.get("collections"))
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default()
}

/// Liczba dokumentow w kolekcji (z read-toola) jako tekst do Tag-a w naglowku workspace.
fn collection_doc_count(id: &str) -> i64 {
    list_collections_data()
        .iter()
        .find(|c| c.get("id").and_then(|v| v.as_str()) == Some(id))
        .and_then(|c| c.get("document_count").and_then(|v| v.as_i64()))
        .unwrap_or(0)
}

// =============================================================================
// Historia czatu w KV (`chat_log:{collection_id}`) — addon nie czyta stanu panelu,
// wiec dymki buduje z KV. Wartosc to JSON array {id, role, content}.
// =============================================================================

/// Klucz KV historii czatu danej bazy (per-sesja, jak pozostale pola panelu).
fn chat_log_key(collection_id: &str) -> String {
    session_key(&format!("chat_log:{collection_id}"))
}

/// Wczytuje historie czatu bazy z KV (pusta gdy brak/niepoprawny JSON).
fn load_chat_log(collection_id: &str) -> Vec<JsonValue> {
    if collection_id.is_empty() {
        return vec![];
    }
    crate::state_get(&chat_log_key(collection_id))
        .ok()
        .flatten()
        .and_then(|b| serde_json::from_slice::<Vec<JsonValue>>(&b).ok())
        .unwrap_or_default()
}

/// Dopisuje wiadomosc {role, content} do historii czatu bazy w KV (Ephemeral).
fn append_chat_message(collection_id: &str, role: &str, content: &str) {
    let mut log = load_chat_log(collection_id);
    let id = log.len();
    log.push(json!({ "id": id, "role": role, "content": content }));
    if let Ok(bytes) = serde_json::to_vec(&log) {
        let _ = crate::state_set(
            &chat_log_key(collection_id),
            &bytes,
            crate::StateTier::Ephemeral,
        );
    }
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

/// Opcje przelacznika warstwy grafu. Wartosci "1"/"0" odpowiadaja parsowaniu w
/// `lib.rs::parse_graph_enabled` ("1"/"true" => on); zapisujemy "1"/"0" dla zwiezlosci.
fn graph_enabled_options() -> Vec<SelectOption> {
    let opt = |value: &str, label: &str| SelectOption {
        value: SelectValue::Text(value.to_string()),
        label: lit(label),
        icon: None,
        disabled: false,
        group_id: None,
        description: None,
    };
    vec![
        opt("0", "Wylaczona (czysty RAG wektorowy)"),
        opt("1", "Wlaczona (MemGraphRAG)"),
    ]
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
            send_workspace(&tab);
            json!({"ok": true, "tab": tab})
        }
        "start-create-collection" => {
            set_kv(SP_CREATE_OPEN, "1");
            send_sidebar();
            json!({"ok": true})
        }
        "cancel-create-collection" => {
            set_kv(SP_CREATE_OPEN, "0");
            set_kv(SP_NEW_COLLECTION, "");
            send_sidebar();
            json!({"ok": true})
        }
        "filter-collections" => {
            // Jeden handler `change` na inpucie: najpierw utrwala value pola (jak
            // `set-field`), potem przerenderowuje sidebar wg aktualnego filtra.
            action_set_field(params);
            send_sidebar();
            json!({"ok": true})
        }
        "set-graph-enabled" => action_set_graph_enabled(params),
        "create-collection" => action_create_collection(params),
        "delete-collection" => action_delete_collection(params),
        "open-collection" => action_open_collection(params),
        "refresh-documents" => {
            send_workspace(TAB_DOCUMENTS);
            json!({"ok": true})
        }
        "delete-document" => action_delete_document(params),
        "ask-question" => action_ask(params),
        "explore-graph" => action_explore_graph(params, false),
        "explore-neighbor" => action_explore_graph(params, true),
        "filter-conflicts" => {
            // Wartosc selecta zostala juz zapisana do KV przez `set-field` (on-change);
            // filtr tylko przerenderowuje zakladke wg aktualnego SP_CONFLICT_STATUS.
            send_workspace(TAB_CONFLICTS);
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

/// Akcja `set-graph-enabled`: utrwala config warstwy grafu (per-instancja) i
/// przerenderowuje CALY shell, bo zmiana wlacza/wylacza zakladki Graf/Konflikty (locked).
/// Wartosc "1"/"true" => on, reszta => off — `lib.rs::graph_enabled()` to JEDYNE zrodlo
/// prawdy dla logiki; tu zapisujemy ten sam klucz instancyjny (Durable, NIE per-sesja:
/// to ustawienie instancji, ma przetrwac restart i obowiazywac wszystkich userow panelu).
fn action_set_graph_enabled(params: &JsonValue) -> JsonValue {
    let raw = params.get("value").and_then(|v| v.as_str()).unwrap_or("0").trim();
    let on = raw.eq_ignore_ascii_case("1") || raw.eq_ignore_ascii_case("true");
    let stored = if on { "1" } else { "0" };
    if let Err(e) = crate::state_set(
        crate::GRAPH_ENABLED_STATE_KEY,
        stored.as_bytes(),
        crate::StateTier::Durable,
    ) {
        return json!({"ok": false, "error": format!("Zapis ustawienia grafu nieudany: {e:?}")});
    }
    // Re-render: sidebar (Badge grafu na kartach) + workspace (NavTabs Graf/Konflikty
    // zmieniaja stan locked, przelacznik pokazuje nowa wartosc) na aktualnej zakladce.
    send_sidebar();
    send_workspace(&active_tab());
    json!({"ok": true, "graph_enabled": on})
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
            | SP_SIDEBAR_SEARCH
            | SP_CHAT_INPUT
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

/// Chirurgiczne odswiezenie widoku dokumentow (tabela + summary + licznik) przez
/// StatePatch — BEZ re-pushu calego slotu workspace. `send_workspace` odbudowuje
/// caly fragment zakladki (upload + lista), co niszczy `tf-file-input` w trakcie
/// uploadu i przerywa masowy upload (pliki po 2. gina po cichu). Tabela bind-uje sie
/// do `SP_DOCUMENT_ROWS`, wiec patch tego pola odswieza WYLACZNIE liste; kontrolka
/// uploadu (bind `upload_files`) zostaje nietknieta i sekwencja uploadu trwa dalej.
fn patch_documents_view(collection: &str) {
    let count = collection_doc_count(collection);
    // KV utrzymany dla hydratacji przy pelnym renderze zakladki; patch dla live UI.
    set_kv(SP_WS_DOCCOUNT, &format!("{count} dok"));
    patch_set(SP_WS_DOCCOUNT, CborValue::Text(format!("{count} dok")));
    patch_set(SP_DOCUMENT_ROWS, load_document_rows(collection));
    patch_set(
        SP_INGEST_SUMMARY,
        CborValue::Text(load_ingest_summary(collection)),
    );
}

/// Akcja `ingest-uploaded`: wpiecie uploadu (upload_complete -> ingest). FileInput
/// emituje `upload_complete` z detail `{doc_ref, filename, mime, name, size}` PO
/// chunked-uploadzie hosta do document store; `doc_ref` to id bloba czytelny przez
/// document_get. Uruchamiamy pelny ingest na wybranej kolekcji (parse->chunk->embedding).
fn action_ingest_uploaded(params: &JsonValue) -> JsonValue {
    let collection = selected_collection();
    if collection.is_empty() {
        patch_status("Najpierw wybierz baze wiedzy w panelu po lewej.");
        return json!({"ok": false, "error": "brak wybranej kolekcji"});
    }
    let (doc_ref, mime, filename) = match parse_upload_detail(params) {
        Ok(parts) => parts,
        Err(msg) => {
            patch_status(&msg);
            return json!({"ok": false, "error": msg});
        }
    };

    // Enqueue-only: zwraca natychmiast (status 'queued'), pipeline mieli w tle
    // scheduled worker `ingest_drain`. Dzieki temu upload_complete nie blokuje
    // polaczenia i masowy upload wielu plikow przechodzi bez czekania na ingest.
    let res = crate::handle_ingest_document(&json!({
        "collection_id": collection,
        "doc_id_blob": doc_ref,
        "filename": filename,
        "mime": mime,
    }));
    if res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let status = res
            .get("data")
            .and_then(|d| d.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("");
        if status == "duplicate" {
            patch_status(&format!("Pominieto duplikat '{filename}' — ta sama tresc juz w bazie."));
        } else {
            patch_status(&format!("Dodano '{filename}' do kolejki ingestu."));
        }
    } else {
        patch_status(&format!("Nie dodano '{filename}': {}", error_text(&res)));
    }
    // Chirurgiczne odswiezenie listy (nowy dokument jako 'pending') — NIE wolno
    // re-pushowac calej zakladki, bo zabija to file-input trwajacego uploadu.
    patch_documents_view(&collection);
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
        patch_status(&format!("Utworzono baze '{name}'."));
        // Zamknij inline-create i wyczysc pole nazwy; nowa kolekcja staje sie aktywna,
        // wiec ustaw wybor i otworz workspace na czacie.
        set_kv(SP_NEW_COLLECTION, "");
        set_kv(SP_CREATE_OPEN, "0");
        if let Some(id) = res
            .get("data")
            .and_then(|d| d.get("collection_id"))
            .and_then(|v| v.as_str())
        {
            select_collection(id, &name);
        }
        send_sidebar();
        send_workspace(TAB_CHAT);
    } else {
        patch_status(&error_text(&res));
        send_sidebar();
    }
    res
}

fn action_delete_collection(_params: &JsonValue) -> JsonValue {
    // Usuwamy AKTUALNIE wybrana baze (przycisk w naglowku workspace), nie wiersz tabeli.
    let id = selected_collection();
    if id.is_empty() {
        return json!({"ok": false, "error": "brak wybranej kolekcji"});
    }
    let res = crate::handle_delete_collection(&json!({ "collection_id": id }));
    if res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        patch_status("Usunieto baze wiedzy.");
        // Wyczysc wybor — workspace wroci do EmptyState.
        set_kv(SP_SELECTED_COLLECTION, "");
        set_kv(SP_SELECTED_COLLECTION_NAME, "");
        set_kv(SP_WS_DOCCOUNT, "");
        send_sidebar();
        send_workspace(TAB_CHAT);
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
    let name = collection_name(&id).unwrap_or_else(|| id.clone());
    select_collection(&id, &name);
    // Re-push sidebar (akcent aktywnej karty) + workspace na czacie (domyslna zakladka).
    send_sidebar();
    send_workspace(TAB_CHAT);
    json!({"ok": true, "collection_id": id})
}

/// Ustawia wybrana baze w KV sesji: id, czysta nazwa i licznik dokumentow do naglowka.
fn select_collection(id: &str, name: &str) {
    set_kv(SP_SELECTED_COLLECTION, id);
    set_kv(SP_SELECTED_COLLECTION_NAME, name);
    refresh_ws_doc_count(id);
}

/// Odswieza licznik dokumentow wybranej bazy (Tag w naglowku workspace).
fn refresh_ws_doc_count(id: &str) {
    let count = collection_doc_count(id);
    set_kv(SP_WS_DOCCOUNT, &format!("{count} dok"));
}

fn action_delete_document(params: &JsonValue) -> JsonValue {
    let id = match row_key(params, "id") {
        Some(id) => id,
        None => return json!({"ok": false, "error": "brak document_id"}),
    };
    let res = crate::handle_delete_document(&json!({ "document_id": id }));
    if res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        patch_status("Usunieto dokument.");
        patch_documents_view(&selected_collection());
    } else {
        patch_status(&error_text(&res));
    }
    res
}

fn action_ask(_params: &JsonValue) -> JsonValue {
    // Pytanie z KV (zapisane przez `set-field`); kolekcja z aktualnego wyboru sidebara.
    let question = match field_value(SP_CHAT_INPUT) {
        Some(q) => q,
        None => {
            patch_status("Wpisz pytanie.");
            return json!({"ok": false, "error": "brak pytania"});
        }
    };
    let collection = selected_collection();
    if collection.is_empty() {
        patch_status("Wybierz baze wiedzy do pytania.");
        return json!({"ok": false, "error": "brak kolekcji"});
    }

    // Dopisz pytanie usera do historii i wyczysc pole wejscia (UI + KV).
    append_chat_message(&collection, "user", &question);
    set_kv(SP_CHAT_INPUT, "");

    patch_status("Pytanie w toku...");
    let res = crate::handle_ask(&json!({ "collection_id": collection, "question": question }));
    let answer = if res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        res.get("data")
            .and_then(|d| d.get("answer"))
            .and_then(|a| a.as_str())
            .unwrap_or("")
            .to_string()
    } else {
        // Blad zapisujemy jako dymek asystenta, zeby kontekst rozmowy byl spojny.
        format!("Blad: {}", error_text(&res))
    };
    append_chat_message(&collection, "assistant", &answer);
    patch_status("Gotowe.");
    // Przebuduj workspace na czacie — dymki czyta z KV historii.
    send_workspace(TAB_CHAT);
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
        send_workspace(TAB_CONFLICTS);
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
        send_workspace(TAB_CONFLICTS);
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
            SP_SIDEBAR_SEARCH,
            SP_CHAT_INPUT,
            SP_GRAPH_QUERY,
            SP_CONFLICT_STATUS,
        ] {
            assert!(is_known_field(f), "pole {f} powinno byc dozwolone");
        }
        // Klucze stanu NIE bedace polami formularza (np. wybrana kolekcja, doc-count) sa
        // odrzucane — set-field nie moze pisac dowolnego klucza KV.
        assert!(!is_known_field(SP_SELECTED_COLLECTION));
        assert!(!is_known_field(SP_WS_DOCCOUNT));
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
            session_key(SP_CHAT_INPUT),
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
            SP_SIDEBAR_SEARCH,
            SP_CHAT_INPUT,
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
