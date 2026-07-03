// =============================================================================
// Plik: addon-sdk/sdk/src/lib.rs
// Opis: TentaFlow Addon SDK — bindingi do host functions, helpery pamieciowe,
//       wysokopoziomowe wrappery do komunikacji z Core (LLM, storage, HTTP,
//       eventy, UI, sekrety, logi, rejestracja narzedzi).
// =============================================================================

//! # Typed UI primitives
//!
//! For new addons, use [`render_panel_typed`] together with `ui::PanelTree`
//! and the typed component sub-enums (`ui::layout::*`, `ui::container::*`,
//! `ui::data_display::*`, etc.). This eliminates the intermediate
//! `serde_json::Value` allocation that `render_panel(panel_id, json!({}))`
//! performs — addons measured a ~5× reduction in guest CPU time and ~3×
//! fewer heap allocations per render. See `notes/addon-ui-perf-plan.md` in
//! the TentaFlow repo for the diagnosis and migration plan.
//!
//! The legacy [`render_panel`] entry point that accepts `serde_json::Value`
//! is preserved for addons that have not yet migrated.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Typed UI schema (PanelTree, UiComponent, theme tokens, layout/container/
/// form/feedback/action/data_display/specialized sub-enums). Re-exported
/// 1:1 from `tentaflow-ui-schema`, the same crate the host links against —
/// so a `PanelTree` value produced here serializes byte-for-byte to what
/// the host expects.
pub mod ui {
    pub use tentaflow_ui_schema::*;
}

// =============================================================================
// AbiError — kanoniczne kody bledow ABI dla F1a host functions
// =============================================================================
//
// MUST stay in sync with `tentaflow-core/src/addon/errors.rs`. The SDK is
// compiled for `wasm32-wasip1` and cannot depend on `tentaflow-core` (the
// core crate pulls in rusqlite, wasmtime, axum, tokio — none of which
// build for that target). Duplicating the enum is the only viable path.
//
// Numeric values are part of the ABI: if you change one, both the host
// and every shipped addon WASM must be rebuilt. The test
// `abi_error_codes_match_plan_spec` in core/errors.rs anchors the
// canonical values (0, 1, 6, 21, 24); the rest are sequential.

/// Kanoniczne kody bledow ABI zwracane przez host functions F1a (SQL,
/// Alias, Camera, Streaming, Recording). Wartosci 0..=24, gdzie 0 = sukces.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiError {
    Ok = 0,
    Permission = 1,
    NotFound = 2,
    NoAvailableTarget = 3,
    Timeout = 4,
    Operation = 5,
    OutputBufferTooSmall = 6,
    Conflict = 7,
    SqlSyntax = 8,
    SqlConstraint = 9,
    SqlNoResult = 10,
    QuotaExceeded = 11,
    CameraUnreachable = 12,
    CameraAuthFailed = 13,
    CameraVendorUnsupported = 14,
    StreamNotFound = 15,
    StreamClosed = 16,
    Backpressure = 17,
    RecordingNotFound = 18,
    RecordingPurged = 19,
    RecordingTimeOutOfRing = 20,
    PayloadTooLarge = 21,
    GateNotSatisfied = 22,
    FrameTokenInvalid = 23,
    FramePurged = 24,
}

impl AbiError {
    /// Wartosc i32 do return z host functions.
    #[inline]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Decodes a raw i32 returned by a host function. Unknown codes fall
    /// back to `Operation` so callers never see a phantom variant after
    /// a host/SDK version skew.
    pub fn from_i32(rc: i32) -> Self {
        match rc {
            0 => Self::Ok,
            1 => Self::Permission,
            2 => Self::NotFound,
            3 => Self::NoAvailableTarget,
            4 => Self::Timeout,
            5 => Self::Operation,
            6 => Self::OutputBufferTooSmall,
            7 => Self::Conflict,
            8 => Self::SqlSyntax,
            9 => Self::SqlConstraint,
            10 => Self::SqlNoResult,
            11 => Self::QuotaExceeded,
            12 => Self::CameraUnreachable,
            13 => Self::CameraAuthFailed,
            14 => Self::CameraVendorUnsupported,
            15 => Self::StreamNotFound,
            16 => Self::StreamClosed,
            17 => Self::Backpressure,
            18 => Self::RecordingNotFound,
            19 => Self::RecordingPurged,
            20 => Self::RecordingTimeOutOfRing,
            21 => Self::PayloadTooLarge,
            22 => Self::GateNotSatisfied,
            23 => Self::FrameTokenInvalid,
            24 => Self::FramePurged,
            _ => Self::Operation,
        }
    }
}

impl From<AbiError> for i32 {
    #[inline]
    fn from(e: AbiError) -> Self {
        e as i32
    }
}

impl core::fmt::Display for AbiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "AbiError({})", *self as i32)
    }
}

// =============================================================================
// Bindingi do host functions (importowane z Core przez WASM)
// =============================================================================

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    /// Generowanie tekstu przez LLM
    /// ABI: (prompt_ptr, prompt_len, model_ptr, model_len, options_ptr, options_len, out_ptr, out_cap, out_len_ptr) -> i32
    fn llm_generate(
        prompt_ptr: i32, prompt_len: i32,
        model_ptr: i32, model_len: i32,
        options_ptr: i32, options_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Odczyt z klucz-wartosc storage
    /// ABI: (key_ptr, key_len, out_ptr, out_cap, out_len_ptr) -> i32
    /// Zgodne z host function w host_functions/storage.rs
    fn storage_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;

    /// Zapis do klucz-wartosc storage
    fn storage_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) -> i32;

    /// Shared in-memory state API (A3) — host-side AddonStateStore exposed to
    /// every instance of the SAME addon. Scoped to the calling addon_id only.
    /// Requires `state.read` (get/list) / `state.write` (set/delete).
    /// state_get: (key_ptr, key_len, out_ptr, out_cap, out_len_ptr) -> i32
    fn state_get_v1(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    /// state_set: CBOR `StateSetInput { key, value, tier }` -> i32
    fn state_set_v1(in_ptr: i32, in_len: i32) -> i32;
    /// state_delete: (key_ptr, key_len) -> 1 (deleted) / 0 (absent) / err
    fn state_delete_v1(key_ptr: i32, key_len: i32) -> i32;
    /// state_list: (prefix_ptr, prefix_len, out_ptr, out_cap, out_len_ptr) -> i32
    /// Output is CBOR `StateListOutput`.
    fn state_list_v1(
        prefix_ptr: i32, prefix_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn sync_acl_upsert_v1(payload_ptr: i32, payload_len: i32) -> i32;

    fn sync_acl_delete_v1(payload_ptr: i32, payload_len: i32) -> i32;

    fn sync_share_grant_v1(payload_ptr: i32, payload_len: i32) -> i32;

    fn sync_share_revoke_v1(payload_ptr: i32, payload_len: i32) -> i32;

    /// Wykonanie requestu HTTP
    /// ABI: (req_ptr, req_len, out_ptr, out_cap, out_len_ptr) -> i32
    fn http_request(req_ptr: i32, req_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;

    /// Publikacja eventu na event bus
    /// ABI: (event_type_ptr, event_type_len, payload_json_ptr, payload_json_len) -> i32
    /// Zgodne z host function w host_functions/events.rs::event_publish
    fn event_publish(
        event_type_ptr: i32, event_type_len: i32,
        payload_json_ptr: i32, payload_json_len: i32,
    ) -> i32;

    /// Subskrypcja eventu — Core wywola guest export `on_event(ptr, len)` przy dostarczeniu.
    /// ABI: (event_type_ptr, event_type_len, filter_json_ptr, filter_json_len) -> i32
    /// Zwraca: subscription_id (>0) lub kod bledu (<0). Filtr opcjonalny — przekaz (0,0).
    fn event_subscribe(
        event_type_ptr: i32, event_type_len: i32,
        filter_json_ptr: i32, filter_json_len: i32,
    ) -> i32;

    /// Renderowanie panelu UI (deklaratywny JSON)
    /// ABI: (panel_id_ptr, panel_id_len, ui_json_ptr, ui_json_len) -> i32
    /// Zgodne z host function w host_functions/ui.rs::ui_render
    fn ui_render(
        panel_id_ptr: i32, panel_id_len: i32,
        ui_json_ptr: i32, ui_json_len: i32,
    ) -> i32;

    /// Renderowanie panelu UI — binary protocol (bincode-encoded PanelTree).
    /// Pozwala pominac JSON serialize/parse w sciezce addon→host
    /// (ok. 20× szybsze, payload ~2-3× mniejszy).
    /// ABI: (panel_id_ptr, panel_id_len, binary_ptr, binary_len) -> i32
    /// Zgodne z host function w host_functions/ui.rs::ui_render_binary
    fn ui_render_binary(
        panel_id_ptr: i32, panel_id_len: i32,
        binary_ptr: i32, binary_len: i32,
    ) -> i32;

    /// Wyswietlenie powiadomienia
    /// ABI: (title_ptr, title_len, body_ptr, body_len, level_ptr, level_len) -> i32
    /// Zgodne z host function w host_functions/ui.rs::ui_notify
    fn ui_notify(
        title_ptr: i32, title_len: i32,
        body_ptr: i32, body_len: i32,
        level_ptr: i32, level_len: i32,
    ) -> i32;

    /// Odczyt sekretu (szyfrowany w Core)
    /// ABI: (key_ptr, key_len, out_ptr, out_cap, out_len_ptr) -> i32
    fn secret_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;

    /// Zapis sekretu
    fn secret_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) -> i32;

    /// Logowanie — poziom info
    fn log_info(msg_ptr: i32, msg_len: i32) -> i32;

    /// Logowanie — poziom warn
    fn log_warn(msg_ptr: i32, msg_len: i32) -> i32;

    /// Logowanie — poziom error
    fn log_error(msg_ptr: i32, msg_len: i32) -> i32;

    /// Pobranie danych aktualnego uzytkownika (JSON)
    /// ABI: (out_ptr, out_cap, out_len_ptr) -> i32
    fn user_get_current(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;

    /// Rejestracja narzedzia (tool) dla LLM
    fn tool_register(def_ptr: i32, def_len: i32) -> i32;

    /// Nawiazanie polaczenia sieciowego TCP/UDP wedlug reguly z manifestu
    fn net_connect(rule_id_ptr: i32, rule_id_len: i32) -> i32;

    /// Wyslanie danych przez aktywne polaczenie sieciowe
    fn net_send(conn_id: i32, data_ptr: i32, data_len: i32) -> i32;

    /// Odebranie danych z aktywnego polaczenia sieciowego
    /// Zwraca packed i64: (status << 32) | bytes_read
    fn net_recv(conn_id: i32, out_ptr: i32, out_capacity: i32) -> i64;

    /// Zamkniecie aktywnego polaczenia sieciowego
    fn net_close(conn_id: i32) -> i32;

    /// Wyslanie requestu do zarejestrowanego serwisu QUIC przez router
    /// ABI: (service_ptr, service_len, request_ptr, request_len, out_ptr, out_cap, out_len_ptr) -> i32
    fn service_request(
        service_ptr: i32, service_len: i32,
        request_ptr: i32, request_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// SQL API (F1a M1.W4) — per-addon SQLite z bindowanymi parametrami.
    /// Zob. `docs/ADDON_HOST_FUNCTIONS.md` sekcja 11 dla pelnej specyfikacji.
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

    fn sql_transaction_v1(
        statements_json_ptr: i32, statements_json_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Vector API (F1c P3) — embedded HNSW per-addon per-namespace vector
    /// indexes (usearch + mmap on disk). Requires `vector.read` (search) /
    /// `vector.write` (upsert/delete) permissions. Wire format is TOML;
    /// vector payloads are base64-encoded little-endian f32 bytes.
    fn vector_upsert_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn vector_search_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn vector_hybrid_search_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn vector_delete_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Document parse API (RAG E1.2) — parsuje OBRAZ strony dokumentu na
    /// markdown + bloki layoutu przez serwis vision-parse (alias `rag-parse`).
    /// Wymaga `document.parse`. Wire format: CBOR; obraz jako base64.
    fn doc_parse_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Ingest-as-flow API (RAG Partia 3) — uruchamia flow `<model>:ingest` z
    /// BINARNYM dokumentem. Addon podaje `doc_id_blob` (referencja do document
    /// store), a host pobiera bajty po swojej stronie i seeduje binarny envelope.
    /// Wymaga `document.read`. Wire format: CBOR (`IngestInvokeInput`/`Output`).
    fn ingest_invoke_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Document/blob store API (RAG E1.3) — per-instance store for user-uploaded
    /// files (PDF/image > the 1 MB KV ceiling). Requires `document.read`
    /// (get/list) / `document.write` (put/delete). CBOR carries only chunk
    /// metadata; raw chunk bytes cross a SEPARATE ptr/len, so a multi-MB file is
    /// not bounded by the CBOR payload ceiling. `document_put_v1` takes the
    /// metadata input plus a chunk ptr/len; `document_get_v1` writes chunk bytes
    /// to `blob_out_ptr` and metadata CBOR to `meta_out_ptr`.
    fn document_put_v1(
        input_ptr: i32, input_len: i32,
        chunk_ptr: i32, chunk_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn document_get_v1(
        input_ptr: i32, input_len: i32,
        blob_out_ptr: i32, blob_out_cap: i32,
        meta_out_ptr: i32, meta_out_cap: i32, meta_out_len_ptr: i32,
    ) -> i32;

    fn document_delete_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn document_list_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Graph API (RAG 0.2) — per-addon per-collection embedded CozoDB graphs.
    /// Requires `graph.read` (neighbors/pagerank/ppr) / `graph.write`
    /// (upsert/delete). Wire format is CBOR. The addon only gets host-shaped,
    /// capped primitives — there is no raw Datalog surface.
    fn graph_upsert_node_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn graph_upsert_edge_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn graph_neighbors_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn graph_pagerank_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn graph_ppr_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn graph_delete_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Web research API — search, read public URLs and read search results.
    /// Wire format is JSON so addons can pass provider-specific options
    /// without recompiling the ABI.
    fn web_research_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Policy / Gate API (F1c P4) — verify a DPIA / FRIA claim against a
    /// `[[gate]]` declaration before running a gated operation. Requires
    /// `policy.read` permission.
    fn gate_check_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Flow API (F1c P5) — invoke addon-declared flow templates, poll status,
    /// request cooperative cancellation. Requires `flow.invoke` permission.
    fn flow_invoke_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn flow_status_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn flow_cancel_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Services catalog API (F2 P2.a) — read-only view of the mesh-wide
    /// service registry. Requires `service.read` permission.
    fn service_list_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn node_resources_get_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Alias API (F1a M1.W5) — readonly inspection of aliases.
    /// Requires `alias.read` permission. Lifecycle (create/deactivate) is
    /// driven implicitly by addon install/uninstall from the manifest.
    fn alias_get_v1(
        alias_id_ptr: i32, alias_id_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn alias_list_owned_v1(
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn alias_list_available_v1(
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Camera API (F1a M1.W6) — camera ingest layer (fake_file vendor only
    /// in F1a). Payload format is CBOR for all inputs/outputs. Requires
    /// `cameras.read` / `cameras.write` / `cameras.snapshot` permissions.
    fn camera_add_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_list_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_analysis_flows_list_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_cv_pipelines_list_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_cv_pipeline_get_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_cv_pipeline_save_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_cv_pipeline_delete_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_get_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_update_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_remove_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_snapshot_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_health_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_discover_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_test_connection_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_credentials_rotate_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Streaming API (F1a M1.W7) — frame bus + PickupToken. Frame bytes are
    /// NOT inlined in `stream_next` output; the addon receives `frame_ref`
    /// + metadata and uses `service_call` to hand the frame to a service.
    fn stream_subscribe_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn stream_next_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn stream_close_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Generic WebRTC channel API (host feature "webrtc"). CBOR I/O. The addon
    /// drives signaling + the data channel; the host owns the native peer.
    fn webrtc_connect_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn webrtc_set_answer_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn webrtc_state_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn webrtc_send_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn webrtc_drain_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn webrtc_close_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn webrtc_register_camera_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Recording API (F1a M1.W8) — snapshot PNG, segment MP4, signed URLs.
    /// All inputs / outputs are TOML. Requires `recording.read` / `recording.write`.
    fn recording_save_snapshot_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn recording_save_segment_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn recording_get_url_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn recording_get_stream_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn recording_purge_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn recording_stats_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn frame_url_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// ONVIF analytics metadata (F2 P6.b) — subscribe/poll/unsubscribe to
    /// per-camera object detection events pulled from the camera's
    /// PullPoint events service. All payloads are TOML.
    fn camera_metadata_subscribe_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_metadata_unsubscribe_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_metadata_poll_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Resize obrazu RGB24 (SIMD downscale). Surowe bajty src plyna bez CBOR;
    /// wymiary jako parametry. Wynik RGB24 dst_w*dst_h*3 do bufora wyjsciowego.
    /// ABI: (src_ptr, src_len, src_w, src_h, dst_w, dst_h, out_ptr, out_cap, out_len_ptr) -> i32
    fn image_resize_rgb_v1(
        src_ptr: i32, src_len: i32,
        src_w: i32, src_h: i32, dst_w: i32, dst_h: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
}

// =============================================================================
// Helpery pamieciowe — odczyt/zapis stringow z pamieci WASM
// =============================================================================

/// Odczytuje string z pamieci guest WASM pod podanym adresem i dlugosci.
/// Uzywane do dekodowania danych przekazanych z hosta.
pub fn read_string(ptr: i32, len: i32) -> String {
    if len <= 0 {
        return String::new();
    }
    let slice = unsafe {
        std::slice::from_raw_parts(ptr as *const u8, len as usize)
    };
    String::from_utf8_lossy(slice).to_string()
}

/// Zapisuje string do bufora w pamieci guest WASM.
/// Zwraca liczbe zapisanych bajtow lub -1 jesli bufor za maly.
pub fn write_string(ptr: i32, max: i32, s: &str) -> i32 {
    let bytes = s.as_bytes();
    if bytes.len() > max as usize {
        return -1;
    }
    let dest = unsafe {
        std::slice::from_raw_parts_mut(ptr as *mut u8, max as usize)
    };
    dest[..bytes.len()].copy_from_slice(bytes);
    bytes.len() as i32
}

// =============================================================================
// Wewnetrzne helpery do wywolywania host functions
// =============================================================================

/// Bufor roboczy na odpowiedzi z hosta (64KB)
const RESPONSE_BUFFER_SIZE: usize = 65536;

/// Wywoluje host function ktora przyjmuje (ptr, len, out_ptr, out_cap, out_len_ptr) -> i32.
/// ABI 5-param: wejscie + bufor wyjsciowy z out_len_ptr.
fn call_host_with_input_and_output_5(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32) -> i32,
    input: &str,
) -> Result<String, String> {
    let input_bytes = input.as_bytes();
    let mut buffer = vec![0u8; RESPONSE_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let result_code = unsafe {
        host_fn(
            input_bytes.as_ptr() as i32,
            input_bytes.len() as i32,
            buffer.as_mut_ptr() as i32,
            RESPONSE_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if result_code < 0 {
        return Err(format!("Host function zwrocila blad: {}", result_code));
    }

    if out_len <= 0 {
        return Ok(String::new());
    }

    let output = String::from_utf8_lossy(&buffer[..out_len as usize]).to_string();
    Ok(output)
}

/// Wywoluje host function ktora przyjmuje dwa pary (ptr, len) (klucz + wartosc).
fn call_host_kv_set(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32) -> i32,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let key_bytes = key.as_bytes();
    let val_bytes = value.as_bytes();

    let result = unsafe {
        host_fn(
            key_bytes.as_ptr() as i32,
            key_bytes.len() as i32,
            val_bytes.as_ptr() as i32,
            val_bytes.len() as i32,
        )
    };

    if result != 0 {
        return Err(format!("Host function zwrocila blad: {}", result));
    }

    Ok(())
}

fn call_host_binary_status(
    host_fn: unsafe extern "C" fn(i32, i32) -> i32,
    payload: &[u8],
) -> Result<(), AbiError> {
    let rc = unsafe { host_fn(payload.as_ptr() as i32, payload.len() as i32) };
    if rc == 0 {
        Ok(())
    } else {
        Err(AbiError::from_i32(rc))
    }
}

/// Wywoluje host function ktora przyjmuje klucz i zwraca wartosc do bufora.
/// ABI 5-param: (key_ptr, key_len, out_ptr, out_cap, out_len_ptr) -> i32
/// Host zapisuje dane do out_ptr i dlugosc do out_len_ptr (4 bajty LE).
/// Zwraca ABI_OK (0), ABI_ERR_NOT_FOUND (-5) lub inny kod bledu.
fn call_host_kv_get_5(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32) -> i32,
    key: &str,
) -> Result<Option<String>, String> {
    let key_bytes = key.as_bytes();
    let mut buffer = vec![0u8; RESPONSE_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let result_code = unsafe {
        host_fn(
            key_bytes.as_ptr() as i32,
            key_bytes.len() as i32,
            buffer.as_mut_ptr() as i32,
            RESPONSE_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    // ABI_ERR_NOT_FOUND = -5
    if result_code == -5 {
        return Ok(None);
    }

    if result_code < 0 {
        return Err(format!("Host function zwrocila blad: {}", result_code));
    }

    // ABI_OK = 0, dlugosc w out_len
    if out_len <= 0 {
        return Ok(None);
    }

    let output = String::from_utf8_lossy(&buffer[..out_len as usize]).to_string();
    Ok(Some(output))
}

/// Wywoluje host function ktora przyjmuje klucz i zwraca wartosc do bufora (4-param ABI).
/// Uzywane przez secret_get ktory ma ABI 4-param.
fn call_host_kv_get(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32) -> i32,
    key: &str,
) -> Result<Option<String>, String> {
    let key_bytes = key.as_bytes();
    let mut buffer = vec![0u8; RESPONSE_BUFFER_SIZE];

    let result_len = unsafe {
        host_fn(
            key_bytes.as_ptr() as i32,
            key_bytes.len() as i32,
            buffer.as_mut_ptr() as i32,
            RESPONSE_BUFFER_SIZE as i32,
        )
    };

    if result_len < 0 {
        // -1 = klucz nie znaleziony (nie blad)
        if result_len == -1 {
            return Ok(None);
        }
        return Err(format!("Host function zwrocila blad: {}", result_len));
    }

    if result_len == 0 {
        return Ok(None);
    }

    let output = String::from_utf8_lossy(&buffer[..result_len as usize]).to_string();
    Ok(Some(output))
}

/// Wywoluje host function do logowania (ptr, len).
fn call_host_log(
    host_fn: unsafe extern "C" fn(i32, i32) -> i32,
    message: &str,
) {
    let bytes = message.as_bytes();
    unsafe {
        host_fn(bytes.as_ptr() as i32, bytes.len() as i32);
    }
}

// =============================================================================
// Wysokopoziomowe wrappery — LLM
// =============================================================================

/// Generuje tekst przez LLM dostepny w Core.
/// Wymaga uprawnienia "llm" w manifescie addonu.
pub fn generate(prompt: &str) -> Result<String, String> {
    let prompt_bytes = prompt.as_bytes();
    let mut buffer = vec![0u8; RESPONSE_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let result_code = unsafe {
        llm_generate(
            prompt_bytes.as_ptr() as i32,
            prompt_bytes.len() as i32,
            0, 0,   // model_ptr, model_len — domyslny model
            0, 0,   // options_ptr, options_len — domyslne opcje
            buffer.as_mut_ptr() as i32,
            RESPONSE_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if result_code < 0 {
        return Err(format!("Host function llm_generate zwrocila blad: {}", result_code));
    }

    if out_len <= 0 {
        return Ok(String::new());
    }

    Ok(String::from_utf8_lossy(&buffer[..out_len as usize]).to_string())
}

// =============================================================================
// Wysokopoziomowe wrappery — Storage (klucz-wartosc)
// =============================================================================

/// Odczytuje wartosc z storage addonu.
/// Zwraca None jesli klucz nie istnieje.
/// Wymaga uprawnienia "storage" w manifescie addonu.
pub fn store_get(key: &str) -> Result<Option<String>, String> {
    call_host_kv_get_5(storage_get, key)
}

/// Zapisuje wartosc do storage addonu.
/// Wymaga uprawnienia "storage" z access_level "rw".
pub fn store_set(key: &str, value: &str) -> Result<(), String> {
    call_host_kv_set(storage_set, key, value)
}

// =============================================================================
// Wysokopoziomowe wrappery — Shared state (host-side AddonStateStore, A3)
// =============================================================================

/// Persistence intent of a shared-state entry.
///
/// * `Ephemeral` — RAM-only, never persisted; evicted under the per-addon cap.
/// * `Durable` — RAM-served and flushed to the backing store by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateTier {
    Ephemeral,
    Durable,
}

impl StateTier {
    fn to_wire(self) -> u8 {
        match self {
            StateTier::Ephemeral => tentaflow_sdk_spec::STATE_TIER_EPHEMERAL,
            StateTier::Durable => tentaflow_sdk_spec::STATE_TIER_DURABLE,
        }
    }

    fn from_wire(raw: u8) -> StateTier {
        // Unknown wire values fall back to Ephemeral (the safe, RAM-only intent);
        // a host that returns an unknown tier is a version skew, not a hard error
        // for a read-only metadata view.
        if raw == tentaflow_sdk_spec::STATE_TIER_DURABLE {
            StateTier::Durable
        } else {
            StateTier::Ephemeral
        }
    }
}

/// Errors surfaced by the shared-state wrappers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// Missing `state.read` / `state.write` permission.
    Permission,
    /// Value exceeded the per-value host cap.
    ValueTooLarge,
    /// The write would exceed the calling addon's state quota.
    QuotaExceeded,
    /// Any other host-side failure (malformed call, memory error, ...).
    Other(AbiError),
}

impl From<AbiError> for StateError {
    fn from(e: AbiError) -> Self {
        match e {
            AbiError::Permission => StateError::Permission,
            AbiError::PayloadTooLarge => StateError::ValueTooLarge,
            AbiError::QuotaExceeded => StateError::QuotaExceeded,
            other => StateError::Other(other),
        }
    }
}

/// One entry's metadata returned by `state_list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateEntryMeta {
    pub key: String,
    /// Value byte length.
    pub size: u64,
    pub tier: StateTier,
}

/// Result of `state_list` — the matching entries plus a `truncated` flag the host
/// sets when the shard had more entries than one call may return (DoS guard).
/// When `truncated` is true the addon should narrow its prefix to page further.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateListResult {
    pub entries: Vec<StateEntryMeta>,
    pub truncated: bool,
}

/// Reads a value from the calling addon's shared state. Returns `Ok(None)` when
/// the key is absent (a normal outcome); a permission or host error is returned
/// as `Err` so the addon never confuses "denied" with "missing". Requires
/// `state.read`.
pub fn state_get(key: &str) -> Result<Option<Vec<u8>>, StateError> {
    match call_sql_with_one_input(state_get_v1, key.as_bytes()) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(AbiError::NotFound) => Ok(None),
        Err(e) => Err(StateError::from(e)),
    }
}

/// Writes a value into the calling addon's shared state under `tier`. Requires
/// `state.write`.
pub fn state_set(key: &str, value: &[u8], tier: StateTier) -> Result<(), StateError> {
    let input = tentaflow_sdk_spec::StateSetInput {
        key: key.to_string(),
        value: value.to_vec(),
        tier: tier.to_wire(),
    };
    let payload = encode_cbor_input(&input)?;
    call_host_binary_status(state_set_v1, &payload).map_err(StateError::from)
}

/// Removes a key from the calling addon's shared state. Returns `Ok(true)` if the
/// key existed, `Ok(false)` if it was absent, and `Err` on a permission/host
/// failure (never silently swallowed). Requires `state.write`.
pub fn state_delete(key: &str) -> Result<bool, StateError> {
    let key_bytes = key.as_bytes();
    let rc = unsafe { state_delete_v1(key_bytes.as_ptr() as i32, key_bytes.len() as i32) };
    match rc {
        1 => Ok(true),
        0 => Ok(false),
        other => Err(StateError::from(AbiError::from_i32(other))),
    }
}

/// Lists `{key, size, tier}` for every key under `prefix` (or all keys when
/// `prefix` is `None`), scoped to the calling addon. Requires `state.read`. A
/// permission or host error is returned as `Err`; the `truncated` flag tells the
/// addon the host clipped the result.
///
/// Uses the 8 MiB output cap so a host-legal response (the host budgets the list
/// well under that) is never silently dropped as an over-cap empty.
pub fn state_list(prefix: Option<&str>) -> Result<StateListResult, StateError> {
    let prefix_bytes = prefix.unwrap_or("").as_bytes();
    let bytes = call_sql_with_one_input_capped(state_list_v1, prefix_bytes, MAX_OUT_CAP_STATE_LIST)
        .map_err(StateError::from)?;
    let out: tentaflow_sdk_spec::StateListOutput =
        decode_cbor(&bytes).map_err(StateError::from)?;
    let entries = out
        .entries
        .into_iter()
        .map(|e| StateEntryMeta {
            key: e.key,
            size: e.size,
            tier: StateTier::from_wire(e.tier),
        })
        .collect();
    Ok(StateListResult {
        entries,
        truncated: out.truncated,
    })
}

// =============================================================================
// Wysokopoziomowe wrappery — Sync ACL
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncAclUpsert {
    pub resource_type: String,
    pub resource_id: String,
    pub owner_user_id: Option<i64>,
    pub assigned_user_id: Option<i64>,
    pub department_id: Option<String>,
    pub manager_user_id: Option<i64>,
    pub visibility_scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncAclDelete {
    pub resource_type: String,
    pub resource_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncShare {
    pub resource_type: String,
    pub resource_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub action: String,
}

pub fn sync_acl_upsert(request: &SyncAclUpsert) -> Result<(), AbiError> {
    let payload = encode_cbor(request)?;
    call_host_binary_status(sync_acl_upsert_v1, &payload)
}

pub fn sync_acl_delete(request: &SyncAclDelete) -> Result<(), AbiError> {
    let payload = encode_cbor(request)?;
    call_host_binary_status(sync_acl_delete_v1, &payload)
}

pub fn sync_share_grant(request: &SyncShare) -> Result<(), AbiError> {
    let payload = encode_cbor(request)?;
    call_host_binary_status(sync_share_grant_v1, &payload)
}

pub fn sync_share_revoke(request: &SyncShare) -> Result<(), AbiError> {
    let payload = encode_cbor(request)?;
    call_host_binary_status(sync_share_revoke_v1, &payload)
}

// =============================================================================
// Wysokopoziomowe wrappery — HTTP
// =============================================================================

/// Definicja requestu HTTP
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
}

/// Odpowiedz HTTP z hosta
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: std::collections::HashMap<String, String>,
    pub body: String,
}

/// Wykonuje request HTTP GET.
/// Wymaga uprawnienia "http" w manifescie addonu.
pub fn http_get(url: &str) -> Result<String, String> {
    let req = HttpRequest {
        method: "GET".to_string(),
        url: url.to_string(),
        headers: std::collections::HashMap::new(),
        body: None,
    };
    let req_json = serde_json::to_string(&req)
        .map_err(|e| format!("Blad serializacji requestu HTTP: {}", e))?;
    call_host_with_input_and_output_5(http_request, &req_json)
}

/// Wykonuje request HTTP POST z podanym body.
/// Wymaga uprawnienia "http" w manifescie addonu.
pub fn http_post(url: &str, body: &str, content_type: &str) -> Result<String, String> {
    let mut headers = std::collections::HashMap::new();
    headers.insert("Content-Type".to_string(), content_type.to_string());

    let req = HttpRequest {
        method: "POST".to_string(),
        url: url.to_string(),
        headers,
        body: Some(body.to_string()),
    };
    let req_json = serde_json::to_string(&req)
        .map_err(|e| format!("Blad serializacji requestu HTTP: {}", e))?;
    call_host_with_input_and_output_5(http_request, &req_json)
}

/// Wykonuje dowolny request HTTP.
/// Wymaga uprawnienia "http" w manifescie addonu.
pub fn http_send(request: &HttpRequest) -> Result<HttpResponse, String> {
    let req_json = serde_json::to_string(request)
        .map_err(|e| format!("Blad serializacji requestu HTTP: {}", e))?;
    let response_str = call_host_with_input_and_output_5(http_request, &req_json)?;
    serde_json::from_str(&response_str)
        .map_err(|e| format!("Blad deserializacji odpowiedzi HTTP: {}", e))
}

// =============================================================================
// Wysokopoziomowe wrappery — Eventy
// =============================================================================

/// Definicja eventu do publikacji
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event_type: String,
    pub payload: serde_json::Value,
}

/// Publikuje event na event bus Core.
/// Wymaga uprawnienia "events" z resource = event_type w manifescie addonu.
pub fn publish_event(event_type: &str, payload: serde_json::Value) -> Result<(), String> {
    let payload_json = serde_json::to_string(&payload)
        .map_err(|e| format!("Blad serializacji payload eventu: {}", e))?;
    let et = event_type.as_bytes();
    let pl = payload_json.as_bytes();
    let result = unsafe {
        event_publish(
            et.as_ptr() as i32, et.len() as i32,
            pl.as_ptr() as i32, pl.len() as i32,
        )
    };
    if result < 0 {
        return Err(format!("Blad publikacji eventu: {}", result));
    }
    Ok(())
}

/// Subskrybuje event — Core wywola guest export `on_event(ptr, len)` przy dostarczeniu.
/// Wymaga uprawnienia "events" z resource = event_type w manifescie addonu.
/// `filter` to opcjonalny filtr JSON (np. dopasowanie polu w payloadzie); `None` = brak filtra.
/// Zwraca `subscription_id` przyznane przez Core.
pub fn subscribe_event(
    event_type: &str,
    filter: Option<serde_json::Value>,
) -> Result<i64, String> {
    let filter_json = match &filter {
        Some(v) => serde_json::to_string(v)
            .map_err(|e| format!("Blad serializacji filtra eventu: {}", e))?,
        None => String::new(),
    };
    let et = event_type.as_bytes();
    let (filter_ptr, filter_len) = if filter.is_some() {
        let fb = filter_json.as_bytes();
        (fb.as_ptr() as i32, fb.len() as i32)
    } else {
        (0i32, 0i32)
    };
    let result = unsafe {
        event_subscribe(
            et.as_ptr() as i32, et.len() as i32,
            filter_ptr, filter_len,
        )
    };
    if result < 0 {
        return Err(format!("Blad subskrypcji eventu: {}", result));
    }
    Ok(result as i64)
}

// =============================================================================
// Wysokopoziomowe wrappery — UI
// =============================================================================

/// Renderuje panel UI addonu (deklaratywny JSON).
/// `content` to drzewo komponentow UI (zgodne z `UiComponent` w Core); panel jest
/// przekazywany do GUI przez event "ui.panel_rendered".
/// Wymaga uprawnienia "ui" w manifescie addonu.
///
/// Legacy entry point — accepts `serde_json::Value`, which forces an extra
/// allocation pass per render. New code should use [`render_panel_typed`]
/// with a typed [`ui::PanelTree`] instead.
pub fn render_panel(panel_id: &str, content: serde_json::Value) -> Result<(), String> {
    let ui_json = serde_json::to_string(&content)
        .map_err(|e| format!("Blad serializacji panelu UI: {}", e))?;
    ui_render_raw(panel_id, &ui_json)
}

/// Renders an addon UI panel from a typed [`ui::PanelTree`].
///
/// Skips the intermediate `serde_json::Value` allocation that
/// [`render_panel`] performs — the tree is serialized straight to JSON in
/// a single pass. Cuts guest-side CPU and allocations versus the legacy
/// `json!({...})` macro pattern (see `notes/addon-ui-perf-plan.md` §2).
/// Requires the `ui` permission in the addon manifest.
pub fn render_panel_typed(panel_id: &str, tree: &ui::PanelTree) -> Result<(), String> {
    let ui_json = serde_json::to_string(tree)
        .map_err(|e| format!("Blad serializacji panelu UI: {}", e))?;
    ui_render_raw(panel_id, &ui_json)
}

/// Renders an addon UI panel by shipping a CBOR-encoded `PanelTree`
/// directly across the addon↔host ABI. No JSON anywhere on the addon side.
///
/// On the host side this drops the `parse_and_validate_panel_tree(&str)`
/// JSON parser entirely — the host decodes CBOR once and
/// goes straight into `validate_panel_tree`. End-to-end this is several
/// times faster than [`render_panel_typed`] for non-trivial trees and the
/// wire payload is ~2-3× smaller (no whitespace, no quoting, integer-
/// tagged field names). See `notes/addon-ui-perf-plan.md` §2 P3.
///
/// Requires the `ui` permission in the addon manifest. Frontend wire
/// format is unchanged (host still serves panels as JSON via `panel_get`).
pub fn render_panel_binary(panel_id: &str, tree: &ui::PanelTree) -> Result<(), String> {
    let encoded = encode_cbor(tree).map_err(|e| format!("Blad serializacji CBOR panelu UI: {e}"))?;
    let pid = panel_id.as_bytes();
    let result = unsafe {
        ui_render_binary(
            pid.as_ptr() as i32, pid.len() as i32,
            encoded.as_ptr() as i32, encoded.len() as i32,
        )
    };
    if result < 0 {
        return Err(format!("Blad renderowania panelu UI (binary): {}", result));
    }
    Ok(())
}

fn encode_cbor<T: Serialize>(value: &T) -> Result<Vec<u8>, AbiError> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes).map_err(|_| AbiError::Operation)?;
    Ok(bytes)
}

/// Internal: ship a pre-serialized panel JSON across the ABI boundary.
/// Shared by [`render_panel`] and [`render_panel_typed`] so the unsafe
/// pointer dance lives in one place.
fn ui_render_raw(panel_id: &str, ui_json: &str) -> Result<(), String> {
    let pid = panel_id.as_bytes();
    let uj = ui_json.as_bytes();
    let result = unsafe {
        ui_render(
            pid.as_ptr() as i32, pid.len() as i32,
            uj.as_ptr() as i32, uj.len() as i32,
        )
    };
    if result < 0 {
        return Err(format!("Blad renderowania panelu UI: {}", result));
    }
    Ok(())
}

/// Wyswietla powiadomienie z poziomem "info".
/// Wymaga uprawnienia "notifications" w manifescie addonu.
pub fn notify(title: &str, body: &str) {
    notify_with_level(title, body, "info");
}

/// Wyswietla powiadomienie z okreslonym poziomem (info, warn, error, success).
pub fn notify_with_level(title: &str, body: &str, level: &str) {
    let t = title.as_bytes();
    let b = body.as_bytes();
    let l = level.as_bytes();
    unsafe {
        ui_notify(
            t.as_ptr() as i32, t.len() as i32,
            b.as_ptr() as i32, b.len() as i32,
            l.as_ptr() as i32, l.len() as i32,
        );
    }
}

// =============================================================================
// Wysokopoziomowe wrappery — Sekrety
// =============================================================================

/// Odczytuje sekret z zaszyfrowanego storage Core.
/// Wymaga uprawnienia "secrets" w manifescie addonu.
pub fn secret_get_value(key: &str) -> Result<Option<String>, String> {
    call_host_kv_get_5(secret_get, key)
}

/// Zapisuje sekret do zaszyfrowanego storage Core.
/// Wymaga uprawnienia "secrets" z access_level "rw".
pub fn secret_set_value(key: &str, value: &str) -> Result<(), String> {
    call_host_kv_set(secret_set, key, value)
}

// =============================================================================
// Wysokopoziomowe wrappery — Logowanie
// =============================================================================

/// Modul logowania — wygodne wrappery do host functions log_*
pub mod log {
    /// Loguje wiadomosc na poziomie INFO
    pub fn info(message: &str) {
        super::call_host_log(super::log_info, message);
    }

    /// Loguje wiadomosc na poziomie WARN
    pub fn warn(message: &str) {
        super::call_host_log(super::log_warn, message);
    }

    /// Loguje wiadomosc na poziomie ERROR
    pub fn error(message: &str) {
        super::call_host_log(super::log_error, message);
    }
}

// =============================================================================
// Wysokopoziomowe wrappery — Uzytkownik
// =============================================================================

/// Dane aktualnego uzytkownika
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentUser {
    pub user_id: i64,
    pub username: String,
    pub display_name: String,
    pub email: String,
}

/// Pobiera dane aktualnego uzytkownika.
/// Wymaga uprawnienia "user_info" w manifescie addonu.
pub fn get_current_user() -> Result<CurrentUser, String> {
    let mut buffer = vec![0u8; RESPONSE_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let result_code = unsafe {
        user_get_current(
            buffer.as_mut_ptr() as i32,
            RESPONSE_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if result_code < 0 {
        return Err(format!("Blad pobierania danych uzytkownika: {}", result_code));
    }

    if out_len <= 0 {
        return Err("Brak danych uzytkownika".to_string());
    }

    let json_str = String::from_utf8_lossy(&buffer[..out_len as usize]).to_string();
    serde_json::from_str(&json_str)
        .map_err(|e| format!("Blad deserializacji danych uzytkownika: {}", e))
}

// =============================================================================
// Wysokopoziomowe wrappery — Rejestracja narzedzi (tool calling)
// =============================================================================

/// Rejestruje narzedzie (tool) dla LLM tool calling.
/// Narzedzie bedzie dostepne w LLM jako function call.
pub fn register_tool(name: &str, description: &str, params_schema: serde_json::Value) {
    let tool_def = serde_json::json!({
        "name": name,
        "description": description,
        "parameters": params_schema,
    });

    if let Ok(json_str) = serde_json::to_string(&tool_def) {
        let bytes = json_str.as_bytes();
        unsafe {
            tool_register(bytes.as_ptr() as i32, bytes.len() as i32);
        }
    }
}

// =============================================================================
// Wysokopoziomowe wrappery — Siec (TCP/UDP proxy)
// =============================================================================

/// Nawiazuje polaczenie sieciowe TCP/UDP wedlug reguly z manifestu.
/// Wymaga uprawnienia "network" w manifescie i zatwierdzenia reguly przez admina.
/// Zwraca conn_id (u32) do uzytku z network_send/network_recv/network_close.
pub fn network_connect(rule_id: &str) -> Result<u32, i32> {
    let bytes = rule_id.as_bytes();
    let result = unsafe {
        net_connect(bytes.as_ptr() as i32, bytes.len() as i32)
    };
    if result < 0 {
        Err(result)
    } else {
        Ok(result as u32)
    }
}

/// Wysyla dane przez aktywne polaczenie sieciowe.
/// Zwraca liczbe wyslanych bajtow.
pub fn network_send(conn_id: u32, data: &[u8]) -> Result<usize, i32> {
    let result = unsafe {
        net_send(conn_id as i32, data.as_ptr() as i32, data.len() as i32)
    };
    if result < 0 {
        Err(result)
    } else {
        Ok(result as usize)
    }
}

/// Odbiera dane z aktywnego polaczenia sieciowego.
/// Dane sa zapisywane do podanego bufora. Zwraca liczbe odebranych bajtow.
pub fn network_recv(conn_id: u32, buf: &mut [u8]) -> Result<usize, i32> {
    let packed = unsafe {
        net_recv(conn_id as i32, buf.as_mut_ptr() as i32, buf.len() as i32)
    };
    // Rozpakuj: status = gorne 32 bity, bytes_read = dolne 32 bity
    let status = (packed >> 32) as i32;
    let bytes_read = (packed & 0xFFFFFFFF) as usize;
    if status < 0 {
        Err(status)
    } else {
        Ok(bytes_read)
    }
}

/// Zamyka aktywne polaczenie sieciowe.
pub fn network_close(conn_id: u32) -> Result<(), i32> {
    let result = unsafe {
        net_close(conn_id as i32)
    };
    if result != 0 {
        Err(result)
    } else {
        Ok(())
    }
}

// =============================================================================
// Wysokopoziomowe wrappery — Service Request (QUIC przez router)
// =============================================================================

/// Wysyla request do zarejestrowanego serwisu QUIC przez router.
/// Wymaga uprawnienia "service" w manifescie.
/// service_name: nazwa serwisu (np. "teams-bot")
/// request_json: JSON payload requestu
/// Zwraca JSON odpowiedzi z serwisu.
pub fn service_request_call(service_name: &str, request_json: &str) -> Result<String, i32> {
    let svc_bytes = service_name.as_bytes();
    let req_bytes = request_json.as_bytes();
    let mut out_buf = vec![0u8; 65536];
    let mut out_len: i32 = 0;

    let result = unsafe {
        service_request(
            svc_bytes.as_ptr() as i32, svc_bytes.len() as i32,
            req_bytes.as_ptr() as i32, req_bytes.len() as i32,
            out_buf.as_mut_ptr() as i32, out_buf.len() as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if result != 0 {
        return Err(result);
    }

    let response = String::from_utf8_lossy(&out_buf[..out_len as usize]).to_string();
    Ok(response)
}

// =============================================================================
// Wysokopoziomowe wrappery — SQL API (F1a M1.W4)
// =============================================================================

/// Reprezentacja wartosci SQL przekazywanej jako parametr lub odebranej
/// z wiersza. Mapowanie 1:1 z ABI (zob. docs sekcja 11):
/// String -> TEXT, I64 -> INTEGER, F64 -> REAL, Bool -> INTEGER 0/1,
/// Null -> NULL, Bytes -> BLOB (przekazywane jako base64 JSON `{"$bytes":"..."}`).
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
    Bytes(Vec<u8>),
}

impl SqlValue {
    /// Reprezentacja JSON kompatybilna z host ABI.
    fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(b) => serde_json::Value::Bool(*b),
            Self::I64(i) => serde_json::Value::from(*i),
            Self::F64(f) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Self::String(s) => serde_json::Value::String(s.clone()),
            Self::Bytes(b) => {
                use base64::Engine;
                let encoded = base64::engine::general_purpose::STANDARD.encode(b);
                serde_json::json!({ "$bytes": encoded })
            }
        }
    }

    fn from_json(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(*b),
            serde_json::Value::Number(n) => n
                .as_i64()
                .map(Self::I64)
                .or_else(|| n.as_f64().map(Self::F64))
                .unwrap_or(Self::Null),
            serde_json::Value::String(s) => Self::String(s.clone()),
            serde_json::Value::Object(obj) => {
                if let Some(serde_json::Value::String(b64)) = obj.get("$bytes") {
                    use base64::Engine;
                    if let Ok(raw) =
                        base64::engine::general_purpose::STANDARD.decode(b64.as_bytes())
                    {
                        return Self::Bytes(raw);
                    }
                }
                Self::Null
            }
            serde_json::Value::Array(_) => Self::Null,
        }
    }

    /// Wygodny dostep do wartosci int.
    pub fn as_i64(&self) -> Option<i64> {
        if let Self::I64(v) = self {
            Some(*v)
        } else {
            None
        }
    }

    /// Wygodny dostep do wartosci string.
    pub fn as_str(&self) -> Option<&str> {
        if let Self::String(s) = self {
            Some(s.as_str())
        } else {
            None
        }
    }
}

/// Wiersz wynikowy SQL — wartosci w kolejnosci kolumn.
pub type SqlRow = Vec<SqlValue>;

/// Wynik DML (sql_exec).
#[derive(Debug, Clone)]
pub struct SqlExecResult {
    pub rows_affected: u64,
    pub last_insert_id: i64,
}

/// Initial buffer for SQL/Alias response (1 KiB — kept small because most
/// responses fit and a retry pulls the actual required size from out_len).
const INITIAL_CAP: usize = 1024;

/// Hard cap on the output buffer for SQL/Alias responses. Matches
/// `PayloadKind::SqlCombined` on the host side. If the response would not
/// fit in this size, the host has misbehaved and we surface PayloadTooLarge
/// rather than allocating unboundedly inside the guest.
const MAX_OUT_CAP: usize = 4 * 1024 * 1024;

/// Hard cap for camera_snapshot responses (RGB24 + base64 expansion). Matches
/// `PayloadKind::ServiceCall` (8 MiB) on the host side. A 1280x720 RGB24 frame
/// is ~3.7 MiB raw → ~4.9 MiB base64-encoded, which would overshoot
/// `MAX_OUT_CAP`; the per-API cap allows the snapshot wrapper to land legit
/// payloads without raising the cap for every other call.
const MAX_OUT_CAP_SNAPSHOT: usize = 8 * 1024 * 1024;

/// Hard cap for `state_list` responses. Matches the host's
/// `PayloadKind::ServiceCall` (8 MiB) ceiling so a host-legal list (which the
/// host budgets well under that) is never silently dropped as over-cap. The
/// generic `MAX_OUT_CAP` (4 MiB) would hide 4-8 MiB legal responses.
const MAX_OUT_CAP_STATE_LIST: usize = 8 * 1024 * 1024;

/// Hard cap for webrtc_drain. The host caps the drained batch at MAX_DRAIN_BYTES
/// (3 MiB raw), which base64-encodes to ~4 MiB plus CBOR overhead — above the
/// generic `MAX_OUT_CAP`. Match the host's `PayloadKind::ServiceCall` (8 MiB) so
/// a legal large drain is never rejected (which would strand the staged batch).
const MAX_OUT_CAP_WEBRTC_DRAIN: usize = 8 * 1024 * 1024;

/// Hard cap dla wyniku resize obrazu (RGB24). Host jest zrodlem prawdy:
/// `image_resize_rgb_v1` odrzuca > `MAX_PIXELS` (64 MP), wiec maks. wynik to
/// 64 MP * 3 bajty/px = 192 MiB. Snapshot kamery (`MAX_OUT_CAP_SNAPSHOT`) to
/// odrebny limit i nie ma zastosowania do resize.
const MAX_IMAGE_OUT_CAP: usize = 64 * 1024 * 1024 * 3;

/// Stream subscribe/next/close responses carry only small metadata payloads
/// (stream_id, frame_ref + a few numeric fields, never frame bytes). 4 KiB is
/// well above the realistic ceiling and keeps the guest from following a
/// misbehaving host into a multi-megabyte allocation.
const MAX_OUT_CAP_STREAM: usize = 4 * 1024;

/// Maksymalna liczba prob retry (bez bedu) na pojedynczym callu.
/// W praktyce 1 attempt = sukces, 2 attempt = sukces po znalezieniu rozmiaru.
/// Trzecia proba sugeruje host bug — zwracamy OutputBufferTooSmall.
const MAX_RETRY_ATTEMPTS: u32 = 2;

/// Wykonuje host function SQL/Alias z retry semantics (out_cap → re-alloc).
/// Retry jest ograniczony przez `MAX_RETRY_ATTEMPTS` i hard-cap `MAX_OUT_CAP`,
/// chroniac guest przed nieograniczonymi alokacjami w przypadku bledu host.
fn call_sql_with_two_inputs(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32) -> i32,
    a: &[u8],
    b: &[u8],
) -> Result<Vec<u8>, AbiError> {
    let mut cap = INITIAL_CAP;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let mut buffer = vec![0u8; cap];
        let mut out_len: u32 = 0;
        let rc = unsafe {
            host_fn(
                a.as_ptr() as i32,
                a.len() as i32,
                b.as_ptr() as i32,
                b.len() as i32,
                buffer.as_mut_ptr() as i32,
                cap as i32,
                &mut out_len as *mut u32 as i32,
            )
        };
        if rc == 0 {
            buffer.truncate(out_len as usize);
            return Ok(buffer);
        }
        if rc == AbiError::OutputBufferTooSmall.as_i32() {
            // Stop retrying after the second attempt: a correct host gives
            // us the required size on the first try, so any further loop
            // is a host bug — fail rather than spin.
            if attempts > MAX_RETRY_ATTEMPTS {
                return Err(AbiError::OutputBufferTooSmall);
            }
            let required = out_len as usize;
            if required <= cap {
                // Host claims too-small but we already meet the size —
                // protocol violation.
                return Err(AbiError::OutputBufferTooSmall);
            }
            if required > MAX_OUT_CAP {
                // Response would exceed the per-API payload limit. Surface
                // PayloadTooLarge so callers can distinguish from a real
                // out_cap negotiation failure.
                return Err(AbiError::PayloadTooLarge);
            }
            cap = required;
            continue;
        }
        return Err(AbiError::from_i32(rc));
    }
}

fn call_sql_with_one_input(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32) -> i32,
    a: &[u8],
) -> Result<Vec<u8>, AbiError> {
    call_sql_with_one_input_capped(host_fn, a, MAX_OUT_CAP)
}

fn call_sql_with_one_input_capped(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32) -> i32,
    a: &[u8],
    max_out_cap: usize,
) -> Result<Vec<u8>, AbiError> {
    let mut cap = INITIAL_CAP;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let mut buffer = vec![0u8; cap];
        let mut out_len: u32 = 0;
        let rc = unsafe {
            host_fn(
                a.as_ptr() as i32,
                a.len() as i32,
                buffer.as_mut_ptr() as i32,
                cap as i32,
                &mut out_len as *mut u32 as i32,
            )
        };
        if rc == 0 {
            buffer.truncate(out_len as usize);
            return Ok(buffer);
        }
        if rc == AbiError::OutputBufferTooSmall.as_i32() {
            if attempts > MAX_RETRY_ATTEMPTS {
                return Err(AbiError::OutputBufferTooSmall);
            }
            let required = out_len as usize;
            if required <= cap {
                return Err(AbiError::OutputBufferTooSmall);
            }
            if required > max_out_cap {
                return Err(AbiError::PayloadTooLarge);
            }
            cap = required;
            continue;
        }
        return Err(AbiError::from_i32(rc));
    }
}

fn params_to_json(params: &[SqlValue]) -> String {
    let arr: Vec<serde_json::Value> = params.iter().map(|v| v.to_json()).collect();
    serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string())
}

/// Wykonuje DML (INSERT/UPDATE/DELETE) z bindowanymi parametrami.
///
/// Wymaga uprawnienia `sql.write` w manifescie oraz `[storage] sql=true`.
/// Bledy zwracane jako `AbiError` (Permission, SqlSyntax, SqlConstraint,
/// Timeout, PayloadTooLarge, ...).
pub fn sql_exec(query: &str, params: &[SqlValue]) -> Result<SqlExecResult, AbiError> {
    let params_json = params_to_json(params);
    let bytes = call_sql_with_two_inputs(sql_exec_v1, query.as_bytes(), params_json.as_bytes())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)?;
    Ok(SqlExecResult {
        rows_affected: v.get("rows_affected").and_then(|x| x.as_u64()).unwrap_or(0),
        last_insert_id: v.get("last_insert_id").and_then(|x| x.as_i64()).unwrap_or(0),
    })
}

/// Wykonuje SELECT (lub WITH/EXPLAIN) i zwraca wszystkie wiersze.
///
/// Wymaga uprawnienia `sql.read` w manifescie oraz `[storage] sql=true`.
pub fn sql_query(query: &str, params: &[SqlValue]) -> Result<Vec<SqlRow>, AbiError> {
    let params_json = params_to_json(params);
    let bytes = call_sql_with_two_inputs(sql_query_v1, query.as_bytes(), params_json.as_bytes())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)?;
    let rows = v.get("rows").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let out: Vec<SqlRow> = rows
        .into_iter()
        .map(|row| {
            row.as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(SqlValue::from_json)
                .collect()
        })
        .collect();
    Ok(out)
}

/// Wykonuje SELECT i zwraca pierwszy wiersz lub None.
pub fn sql_query_one(query: &str, params: &[SqlValue]) -> Result<Option<SqlRow>, AbiError> {
    let params_json = params_to_json(params);
    let bytes =
        call_sql_with_two_inputs(sql_query_one_v1, query.as_bytes(), params_json.as_bytes())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)?;
    match v.get("row") {
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(serde_json::Value::Array(arr)) => Ok(Some(arr.iter().map(SqlValue::from_json).collect())),
        _ => Err(AbiError::Operation),
    }
}

/// Wykonuje liste statementow atomowo. Wszystkie commited lub wszystkie rolled back.
/// Zwraca laczna liczbe `rows_affected` wszystkich statementow.
pub fn sql_transaction(statements: &[(&str, &[SqlValue])]) -> Result<u64, AbiError> {
    let payload = serde_json::json!({
        "statements": statements.iter().map(|(q, p)| {
            serde_json::json!({
                "query": q,
                "params": p.iter().map(|v| v.to_json()).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    });
    let payload_str = serde_json::to_string(&payload).map_err(|_| AbiError::Operation)?;
    let bytes = call_sql_with_one_input(sql_transaction_v1, payload_str.as_bytes())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)?;
    Ok(v.get("rows_affected_total").and_then(|x| x.as_u64()).unwrap_or(0))
}

// =============================================================================
// Wysokopoziomowe wrappery — Web Research API
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchRequest {
    pub query: String,
    #[serde(default = "default_web_search_limit")]
    pub limit: usize,
    #[serde(default)]
    pub provider: serde_json::Value,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub time_range: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebReadUrlRequest {
    pub url: String,
    #[serde(default = "default_web_read_chars")]
    pub max_chars: usize,
    #[serde(default)]
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebReadSearchResultsRequest {
    pub query: String,
    #[serde(default = "default_web_search_limit")]
    pub search_limit: usize,
    #[serde(default = "default_web_read_limit")]
    pub read_limit: usize,
    #[serde(default = "default_web_read_chars")]
    pub max_chars_per_page: usize,
    #[serde(default)]
    pub provider: serde_json::Value,
    #[serde(default)]
    pub mode: String,
}

fn default_web_search_limit() -> usize {
    10
}

fn default_web_read_limit() -> usize {
    5
}

fn default_web_read_chars() -> usize {
    30_000
}

pub fn web_research(request: &serde_json::Value) -> Result<serde_json::Value, AbiError> {
    let payload = serde_json::to_vec(request).map_err(|_| AbiError::Operation)?;
    let bytes = call_sql_with_one_input_capped(web_research_v1, &payload, 8 * 1024 * 1024)?;
    serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)
}

pub fn web_search(request: &WebSearchRequest) -> Result<serde_json::Value, AbiError> {
    web_research(&serde_json::json!({
        "op": "search",
        "query": request.query,
        "limit": request.limit,
        "provider": request.provider,
        "language": request.language,
        "time_range": request.time_range,
    }))
}

pub fn web_read_url(request: &WebReadUrlRequest) -> Result<serde_json::Value, AbiError> {
    let mode = if request.mode.is_empty() { "auto" } else { &request.mode };
    web_research(&serde_json::json!({
        "op": "read_url",
        "url": request.url,
        "max_chars": request.max_chars,
        "mode": mode,
    }))
}

pub fn web_read_search_results(
    request: &WebReadSearchResultsRequest,
) -> Result<serde_json::Value, AbiError> {
    let mode = if request.mode.is_empty() { "auto" } else { &request.mode };
    web_research(&serde_json::json!({
        "op": "read_search_results",
        "query": request.query,
        "search_limit": request.search_limit,
        "read_limit": request.read_limit,
        "max_chars_per_page": request.max_chars_per_page,
        "provider": request.provider,
        "mode": mode,
    }))
}

// =============================================================================
// Prelude — wygodny re-eksport dla autorow addonow
// =============================================================================

/// Prelude — importuj wszystkie najczesciej uzywane typy i funkcje
// =============================================================================
// Generic WebRTC channel wrappers (robot/device transport; host feature "webrtc")
// =============================================================================

/// Config for opening a generic WebRTC channel.
pub struct WebRtcChannelConfig {
    pub data_channel_label: String,
    /// Request an inbound video track (so it can be bound to a camera via
    /// `webrtc_register_camera`).
    pub want_video: bool,
    pub disable_mdns: bool,
    pub gather_timeout_ms: u64,
    pub inbound_capacity: u32,
    /// App-level keepalive for precise RTT: ping text + interval + a substring
    /// identifying the reply. `keepalive_interval_ms = 0` disables it.
    pub keepalive_text: Option<String>,
    pub keepalive_interval_ms: u64,
    pub keepalive_marker: Option<String>,
    /// Target peer IPv4. The host narrows ICE candidate gathering to the local
    /// interface on the peer's subnet (matching the mesh transport selection),
    /// avoiding ICE failures on multi-homed hosts.
    pub peer_ipv4: Option<String>,
}

impl Default for WebRtcChannelConfig {
    fn default() -> Self {
        WebRtcChannelConfig {
            data_channel_label: "data".to_string(),
            want_video: false,
            disable_mdns: true,
            gather_timeout_ms: 8000,
            inbound_capacity: 2048,
            keepalive_text: None,
            keepalive_interval_ms: 0,
            keepalive_marker: None,
            peer_ipv4: None,
        }
    }
}

/// One inbound data-channel message.
pub enum WebRtcInbound {
    Text(String),
    Binary(Vec<u8>),
}

/// Channel/peer readiness snapshot.
pub struct WebRtcChannelState {
    pub peer_state: String,
    pub dc_open: bool,
    pub dropped_count: u64,
    pub queue_len: u32,
    /// Transport round-trip latency in ms; None until first measured.
    pub rtt_ms: Option<f64>,
}

/// Result of a drain poll.
pub struct WebRtcDrained {
    pub messages: Vec<WebRtcInbound>,
    pub dropped_count: u64,
    pub queue_len: u32,
    pub closed: bool,
}

fn b64_encode(d: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(d)
}

fn b64_decode(s: &str) -> Result<Vec<u8>, AbiError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|_| AbiError::Operation)
}

/// Open a channel. Returns `(channel_id, offer_sdp)`. The addon ferries
/// `offer_sdp` to the peer via its own signaling and feeds the answer back to
/// `webrtc_set_answer`.
pub fn webrtc_connect(cfg: &WebRtcChannelConfig) -> Result<(String, String), AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::WebRtcConnectInput {
        data_channel_label: cfg.data_channel_label.clone(),
        want_video: cfg.want_video,
        disable_mdns: cfg.disable_mdns,
        gather_timeout_ms: cfg.gather_timeout_ms,
        inbound_capacity: cfg.inbound_capacity,
        keepalive_text: cfg.keepalive_text.clone(),
        keepalive_interval_ms: cfg.keepalive_interval_ms,
        keepalive_marker: cfg.keepalive_marker.clone(),
        peer_ipv4: cfg.peer_ipv4.clone(),
    })?;
    let bytes = call_sql_with_one_input(webrtc_connect_v1, &payload)?;
    let out: tentaflow_sdk_spec::WebRtcConnectOutput = decode_cbor(&bytes)?;
    Ok((out.channel_id, out.offer_sdp))
}

pub fn webrtc_set_answer(channel_id: &str, answer_sdp: &str) -> Result<(), AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::WebRtcSetAnswerInput {
        channel_id: channel_id.to_string(),
        answer_sdp: answer_sdp.to_string(),
    })?;
    let bytes = call_sql_with_one_input(webrtc_set_answer_v1, &payload)?;
    let _: tentaflow_sdk_spec::WebRtcStatusOutput = decode_cbor(&bytes)?;
    Ok(())
}

pub fn webrtc_state(channel_id: &str) -> Result<WebRtcChannelState, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::WebRtcStateInput {
        channel_id: channel_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(webrtc_state_v1, &payload)?;
    let o: tentaflow_sdk_spec::WebRtcStateOutput = decode_cbor(&bytes)?;
    Ok(WebRtcChannelState {
        peer_state: o.peer_state,
        dc_open: o.dc_open,
        dropped_count: o.dropped_count,
        queue_len: o.queue_len,
        rtt_ms: o.rtt_ms,
    })
}

pub fn webrtc_send_text(channel_id: &str, text: &str) -> Result<(), AbiError> {
    webrtc_send_inner(channel_id, true, text.as_bytes())
}

pub fn webrtc_send_binary(channel_id: &str, data: &[u8]) -> Result<(), AbiError> {
    webrtc_send_inner(channel_id, false, data)
}

fn webrtc_send_inner(channel_id: &str, is_text: bool, data: &[u8]) -> Result<(), AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::WebRtcSendInput {
        channel_id: channel_id.to_string(),
        is_text,
        data_b64: b64_encode(data),
    })?;
    let bytes = call_sql_with_one_input(webrtc_send_v1, &payload)?;
    let _: tentaflow_sdk_spec::WebRtcStatusOutput = decode_cbor(&bytes)?;
    Ok(())
}

/// Poll up to `max_messages` inbound data-channel messages.
pub fn webrtc_drain(channel_id: &str, max_messages: u32) -> Result<WebRtcDrained, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::WebRtcDrainInput {
        channel_id: channel_id.to_string(),
        max_messages,
    })?;
    let bytes =
        call_sql_with_one_input_capped(webrtc_drain_v1, &payload, MAX_OUT_CAP_WEBRTC_DRAIN)?;
    let o: tentaflow_sdk_spec::WebRtcDrainOutput = decode_cbor(&bytes)?;
    let messages = o
        .messages
        .into_iter()
        .map(|m| {
            let raw = b64_decode(&m.data_b64).unwrap_or_default();
            if m.is_text {
                WebRtcInbound::Text(String::from_utf8_lossy(&raw).into_owned())
            } else {
                WebRtcInbound::Binary(raw)
            }
        })
        .collect();
    Ok(WebRtcDrained {
        messages,
        dropped_count: o.dropped_count,
        queue_len: o.queue_len,
        closed: o.closed,
    })
}

pub fn webrtc_close(channel_id: &str) -> Result<(), AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::WebRtcCloseInput {
        channel_id: channel_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(webrtc_close_v1, &payload)?;
    let _: tentaflow_sdk_spec::WebRtcStatusOutput = decode_cbor(&bytes)?;
    Ok(())
}

/// Bind a channel's inbound video track to a camera (consumable by the camera /
/// streaming host functions). Returns the new camera_id.
pub fn webrtc_register_camera(
    channel_id: &str,
    display_name: &str,
    target_fps: u32,
    analysis_fps: u32,
) -> Result<String, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::WebRtcRegisterCameraInput {
        channel_id: channel_id.to_string(),
        display_name: display_name.to_string(),
        target_fps,
        analysis_fps,
        // Generic webrtc cameras don't report their lens; an addon that knows its
        // intrinsics (e.g. go2) constructs the input directly with the FOV set.
        camera_fov_deg: None,
        camera_fov_v_deg: None,
        camera_depth_scale: None,
    })?;
    let bytes = call_sql_with_one_input(webrtc_register_camera_v1, &payload)?;
    let out: tentaflow_sdk_spec::WebRtcRegisterCameraOutput = decode_cbor(&bytes)?;
    Ok(out.camera_id)
}

pub mod prelude {
    pub use crate::ui;
    pub use crate::{
        read_string, write_string,
        generate,
        store_get, store_set,
        state_get, state_set, state_delete, state_list,
        StateTier, StateError, StateEntryMeta, StateListResult,
        http_get, http_post, http_send, HttpRequest, HttpResponse,
        publish_event, subscribe_event, Event,
        render_panel, render_panel_typed, render_panel_binary, notify, notify_with_level,
        secret_get_value, secret_set_value,
        get_current_user, CurrentUser,
        register_tool,
        network_connect, network_send, network_recv, network_close,
        service_request_call,
        sql_exec, sql_query, sql_query_one, sql_transaction,
        SqlValue, SqlRow, SqlExecResult,
        alias_get, alias_list_owned, alias_list_available,
        AliasInfo, AvailableAlias,
        camera_add, camera_list, camera_get, camera_update, camera_remove,
        camera_snapshot, camera_health, camera_discover, camera_test_connection,
        camera_credentials_rotate,
        CameraAddSpec, CameraAddResult, CameraInfo, CameraUpdateSpec,
        CameraHealthInfo, SnapshotInfo, CameraTestResult,
        stream_subscribe, stream_next, stream_close,
        StreamNextMessage, StreamFrameMeta,
        webrtc_connect, webrtc_set_answer, webrtc_state, webrtc_send_text, webrtc_send_binary,
        webrtc_drain, webrtc_close, webrtc_register_camera,
        WebRtcChannelConfig, WebRtcChannelState, WebRtcDrained, WebRtcInbound,
        camera_metadata_subscribe, camera_metadata_poll, camera_metadata_unsubscribe,
        MetadataItem, MetadataFrame, MetadataPollResult,
        recording_save_snapshot, recording_save_segment, recording_get_url,
        recording_get_stream, recording_purge, recording_stats, frame_url,
        SavedRecordingInfo, RecordingUrl, RecordingStream, RecordingStats, FrameUrl,
        vector_upsert, vector_upsert_sparse, vector_search, vector_hybrid_search, vector_delete,
        encode_vector_b64, VectorHit,
        VectorField, VectorFieldType, VectorFieldValue, VectorFilter, VectorFusion, SparseVector,
        graph_upsert_node, graph_upsert_edge, graph_neighbors, graph_pagerank,
        graph_ppr, graph_delete_node, graph_delete_edge, graph_tombstone_node,
        GraphNode, GraphProp, GraphSeed, GraphNeighbor, GraphRankedNode,
        GraphDirection, Provenance,
        web_research, web_search, web_read_url, web_read_search_results,
        WebSearchRequest, WebReadUrlRequest, WebReadSearchResultsRequest,
        gate_check, gate_check_scoped, GateCheckResult, GateSigner,
        flow_invoke, flow_status, flow_cancel, FlowInvocation,
        service_list, node_resources_get, ServiceInfo, NodeResources, NodeGpu,
        AbiError,
        log,
    };
    pub use serde::{Deserialize, Serialize};
    pub use serde_json::{self, json, Value};
}

// =============================================================================
// Wysokopoziomowe wrappery — Aliases API (F1a M1.W5, readonly)
// =============================================================================

/// Pelne info o aliasie zwracane przez `alias_get_v1` i `alias_list_owned_v1`.
#[derive(Debug, Clone, Deserialize)]
pub struct AliasInfo {
    pub id: String,
    /// "addon:<id>" lub "manual" lub None gdy brak owner row.
    pub owner: Option<String>,
    pub current_target: String,
    pub fallback_targets: Vec<String>,
    pub strategy: String,
    pub is_active: bool,
    pub last_used_target: Option<String>,
    pub last_used_at: Option<i64>,
    pub calls_24h: u64,
    pub fallback_calls_24h: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct AliasListResponse {
    aliases: Vec<AliasInfo>,
}

/// Pobiera pelne info o aliasie razem ze statystykami (last_used_*,
/// calls_24h, fallback_calls_24h).
///
/// Read access: dowolny addon z `alias.read` (bez ograniczenia
/// ownership). Stats sa widoczne wylacznie dla wlasciciela aliasu i dla
/// manual-owned aliasow — cross-addon caller dostanie metadata + counters
/// = 0 / last_used_* = null.
pub fn alias_get(alias_id: &str) -> Result<AliasInfo, AbiError> {
    let bytes = call_sql_with_one_input(alias_get_v1, alias_id.as_bytes())?;
    serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)
}

/// Zwraca liste aliasow nalezacych do biezacego addona (owner_id =
/// caller). Inne aliasy (manual, owned by innym addonem) sa pomijane.
pub fn alias_list_owned() -> Result<Vec<AliasInfo>, AbiError> {
    // Host function bez argumentow wejsciowych: invoke direct z retry pattern
    // chronionym przez te same gwarancje (MAX_OUT_CAP, MAX_RETRY_ATTEMPTS) co
    // call_sql_with_*.
    let mut cap = INITIAL_CAP;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let mut buffer = vec![0u8; cap];
        let mut out_len: u32 = 0;
        let rc = unsafe {
            alias_list_owned_v1(
                buffer.as_mut_ptr() as i32,
                cap as i32,
                &mut out_len as *mut u32 as i32,
            )
        };
        if rc == 0 {
            buffer.truncate(out_len as usize);
            let resp: AliasListResponse =
                serde_json::from_slice(&buffer).map_err(|_| AbiError::Operation)?;
            return Ok(resp.aliases);
        }
        if rc == AbiError::OutputBufferTooSmall.as_i32() {
            if attempts > MAX_RETRY_ATTEMPTS {
                return Err(AbiError::OutputBufferTooSmall);
            }
            let required = out_len as usize;
            if required <= cap {
                return Err(AbiError::OutputBufferTooSmall);
            }
            if required > MAX_OUT_CAP {
                return Err(AbiError::PayloadTooLarge);
            }
            cap = required;
            continue;
        }
        return Err(AbiError::from_i32(rc));
    }
}

/// Jeden alias/model, ktory addon MOZE konsumowac — wynik z systemu grantow
/// dostepu (`[[uses_alias]]`). W przeciwienstwie do `AliasInfo` (aliasy, ktore
/// addon STWORZYL), to opisuje aliasy, do ktorych addon dostal grant. Pola
/// `target_model`/`visibility` sa `None` gdy alias jeszcze nie istnieje (owner
/// niezainstalowany, status `pending`).
#[derive(Debug, Clone, Deserialize)]
pub struct AvailableAlias {
    /// Nazwa aliasu zadeklarowana przez addon w `[[uses_alias]]`.
    pub alias_id: String,
    /// Konkretny model, na ktory alias sie rozwiazuje (gdy istnieje).
    pub target_model: Option<String>,
    /// Metody/zdolnosci (detect/recognize/embed/...) zadeklarowane przez owner.
    pub methods: Vec<String>,
    /// Strategia routingu aliasu (gdy istnieje).
    pub strategy: Option<String>,
    /// Status grantu: `granted` / `auto_granted` / `pending` / `denied`.
    pub grant_status: String,
    /// Widocznosc ustawiona przez owner (`private`/`restricted`/`public`).
    pub visibility: Option<String>,
    /// Czy rozwiazany alias jest aktywny.
    pub active: bool,
    /// Czy consumer zadeklarowal alias jako `required`.
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct AvailableAliasResponse {
    aliases: Vec<AvailableAlias>,
}

/// Zwraca aliasy/modele, ktore biezacy addon MOZE konsumowac — jego deklaracje
/// `[[uses_alias]]` zlaczone z konkretnym modelem docelowym, metodami, strategia,
/// widocznoscia i statusem grantu. Lista zawiera WSZYSTKIE statusy (granted/
/// auto_granted/pending/denied), zeby UI mogl pokazac honest stan zamiast ukrywac
/// nieprzyznane wpisy. Realny gate dostepu i tak dziala przy wywolaniu (resolve),
/// wiec wpis `pending`/`denied` na liscie nie przyznaje dostepu.
pub fn alias_list_available() -> Result<Vec<AvailableAlias>, AbiError> {
    let mut cap = INITIAL_CAP;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let mut buffer = vec![0u8; cap];
        let mut out_len: u32 = 0;
        let rc = unsafe {
            alias_list_available_v1(
                buffer.as_mut_ptr() as i32,
                cap as i32,
                &mut out_len as *mut u32 as i32,
            )
        };
        if rc == 0 {
            buffer.truncate(out_len as usize);
            let resp: AvailableAliasResponse =
                serde_json::from_slice(&buffer).map_err(|_| AbiError::Operation)?;
            return Ok(resp.aliases);
        }
        if rc == AbiError::OutputBufferTooSmall.as_i32() {
            if attempts > MAX_RETRY_ATTEMPTS {
                return Err(AbiError::OutputBufferTooSmall);
            }
            let required = out_len as usize;
            if required <= cap {
                return Err(AbiError::OutputBufferTooSmall);
            }
            if required > MAX_OUT_CAP {
                return Err(AbiError::PayloadTooLarge);
            }
            cap = required;
            continue;
        }
        return Err(AbiError::from_i32(rc));
    }
}

// =============================================================================
// Camera API (F1a M1.W6) — TentaVision camera ingest
// =============================================================================
//
// Wrapper-y woke host functions camera_*_v1. Payload to CBOR; bledy mapowane na
// `AbiError`. Pelna specyfikacja: `docs/ADDON_HOST_FUNCTIONS.md` sekcja 13.
//
// **All `camera_*` wrappers require TentaFlow core built with
// `--features camera`.** Without that feature the host does not register the
// imports and addon instantiation fails at module-link time with a
// "missing import" error from wasmtime — there is no silent-fail path.

/// Specyfikacja nowej kamery do `camera_add`. F1a obsluguje wylacznie
/// `vendor = "fake_file"`; pozostale vendor-y dadza `CameraVendorUnsupported`.
#[derive(Debug, Clone)]
pub struct CameraAddSpec {
    pub display_name: String,
    pub vendor: String,
    pub url: String,
    pub target_fps: u32,
    /// Per-camera AI analysis frame rate (`0` = unlimited / native cadence).
    pub analysis_fps: u32,
    pub resolution: Option<(u32, u32)>,
    pub retention_class: String,
    pub profile: String,
    /// Base64-encoded `user:pass` for vendors requiring auth (RTSP/ONVIF).
    /// Required for `vendor = "onvif"`; the host also uses it for the SOAP
    /// UsernameToken digest. `None` for credential-less vendors (`fake_file`).
    pub credentials_b64: Option<String>,
    /// `vendor = "onvif"` only: pins a specific media profile. `None` selects
    /// the first profile returned by `GetProfiles`.
    pub onvif_profile_token: Option<String>,
}

impl Default for CameraAddSpec {
    fn default() -> Self {
        Self {
            display_name: String::new(),
            vendor: "fake_file".to_string(),
            url: String::new(),
            target_fps: 30,
            analysis_fps: 10,
            resolution: None,
            retention_class: "C".to_string(),
            profile: "default".to_string(),
            credentials_b64: None,
            onvif_profile_token: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CameraAddResult {
    pub camera_id: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct CameraInfo {
    pub camera_id: String,
    pub display_name: String,
    pub vendor: String,
    pub url: String,
    pub target_fps: i64,
    pub resolution_width: Option<i64>,
    pub resolution_height: Option<i64>,
    pub status: String,
    pub status_message: Option<String>,
    pub fps_actual: Option<f64>,
    pub last_frame_at: Option<i64>,
    pub retention_class: String,
    pub profile: String,
    /// Per-camera analysis Flow id (None/empty = none assigned).
    pub analysis_flow_id: Option<String>,
    /// Per-camera CV pipeline id (None/empty = the default pipeline).
    pub cv_pipeline_id: Option<String>,
}

impl From<tentaflow_sdk_spec::CameraInfoOut> for CameraInfo {
    fn from(o: tentaflow_sdk_spec::CameraInfoOut) -> Self {
        Self {
            camera_id: o.camera_id,
            display_name: o.display_name,
            vendor: o.vendor,
            url: o.url,
            target_fps: o.target_fps,
            resolution_width: o.resolution_width,
            resolution_height: o.resolution_height,
            status: o.status,
            status_message: o.status_message,
            fps_actual: o.fps_actual,
            last_frame_at: o.last_frame_at,
            retention_class: o.retention_class,
            profile: o.profile,
            analysis_flow_id: o.analysis_flow_id,
            cv_pipeline_id: o.cv_pipeline_id,
        }
    }
}

/// Partial update for `camera_update`. URL i vendor sa nie do zmiany w F1a —
/// rebind wymaga remove + add.
#[derive(Debug, Default, Clone)]
pub struct CameraUpdateSpec {
    pub camera_id: String,
    pub display_name: Option<String>,
    pub target_fps: Option<u32>,
    /// Per-camera AI analysis frame rate (`0` = unlimited); `None` keeps current.
    pub analysis_fps: Option<u32>,
    pub resolution_width: Option<u32>,
    pub resolution_height: Option<u32>,
    pub retention_class: Option<String>,
    pub profile: Option<String>,
    /// Per-camera analysis Flow id. `None` keeps current; `Some("")` clears it;
    /// `Some(id)` assigns (host validates the flow exists and is active).
    pub analysis_flow_id: Option<String>,
    /// Per-camera CV pipeline id. Same tri-state as `analysis_flow_id`:
    /// `None` keeps current, `Some("")` clears (back to the default pipeline),
    /// `Some(id)` assigns (host validates the pipeline exists).
    pub cv_pipeline_id: Option<String>,
}

/// One assignable camera-analysis flow (id + display name) from
/// [`camera_analysis_flows`], for populating the per-camera flow selector.
#[derive(Debug, Clone)]
pub struct CameraAnalysisFlow {
    pub id: String,
    pub name: String,
}

/// One camera CV pipeline summary from [`camera_cv_pipelines_list`], for the
/// per-camera pipeline picker and the pipeline manager list.
#[derive(Debug, Clone)]
pub struct CameraCvPipelineSummary {
    pub id: String,
    pub name: String,
    /// Seed-owned default pipeline — cannot be deleted; cameras without an
    /// explicit assignment resolve to it.
    pub is_default: bool,
    pub updated_at: i64,
}

/// One full pipeline (JSON body included) from [`camera_cv_pipeline_get`].
#[derive(Debug, Clone)]
pub struct CameraCvPipeline {
    pub id: String,
    pub name: String,
    /// `{"stages":[...]}` per the core `cv_pipeline::CvPipeline` schema.
    pub pipeline_json: String,
}

/// Outcome of [`camera_cv_pipeline_save`]. A host-side validation failure
/// (structure or unknown model alias) is NOT an ABI error — it comes back as
/// `id = None` + a human-readable `error` for the UI to display verbatim.
#[derive(Debug, Clone)]
pub struct CameraCvPipelineSaveResult {
    pub id: Option<String>,
    pub error: Option<String>,
}

/// Outcome of [`camera_cv_pipeline_delete`]. A refused delete (default
/// pipeline, still referenced by a camera) comes back as `deleted = false`
/// + a human-readable `error`.
#[derive(Debug, Clone)]
pub struct CameraCvPipelineDeleteResult {
    pub deleted: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CameraHealthInfo {
    pub camera_id: String,
    pub status: String,
    pub status_message: String,
    pub fps_actual: f64,
    pub last_frame_at: i64,
    pub frames_total: u64,
    pub frames_dropped: u64,
}

/// Wynik `camera_snapshot` — RGB24 frame zdekodowany z base64.
#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    pub camera_id: String,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub timestamp_unix_ms: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct CameraTestResult {
    pub ok: bool,
    pub message: String,
}

/// Decodes a CBOR host response into a shared `tentaflow-sdk-spec` ABI struct.
/// `minicbor::decode` stops at the first complete value, so a valid prefix
/// followed by trailing bytes would otherwise be accepted; this requires the
/// whole response to be consumed.
fn decode_cbor<T>(bytes: &[u8]) -> Result<T, AbiError>
where
    T: for<'b> minicbor::Decode<'b, ()>,
{
    let mut decoder = minicbor::Decoder::new(bytes);
    let value = decoder.decode::<T>().map_err(|_| AbiError::Operation)?;
    if decoder.position() != bytes.len() {
        return Err(AbiError::Operation);
    }
    Ok(value)
}

fn call_host_no_input(
    host_fn: unsafe extern "C" fn(i32, i32, i32) -> i32,
) -> Result<Vec<u8>, AbiError> {
    let mut cap = INITIAL_CAP;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let mut buffer = vec![0u8; cap];
        let mut out_len: u32 = 0;
        let rc = unsafe {
            host_fn(
                buffer.as_mut_ptr() as i32,
                cap as i32,
                &mut out_len as *mut u32 as i32,
            )
        };
        if rc == 0 {
            buffer.truncate(out_len as usize);
            return Ok(buffer);
        }
        if rc == AbiError::OutputBufferTooSmall.as_i32() {
            if attempts > MAX_RETRY_ATTEMPTS {
                return Err(AbiError::OutputBufferTooSmall);
            }
            let required = out_len as usize;
            if required <= cap {
                return Err(AbiError::OutputBufferTooSmall);
            }
            if required > MAX_OUT_CAP {
                return Err(AbiError::PayloadTooLarge);
            }
            cap = required;
            continue;
        }
        return Err(AbiError::from_i32(rc));
    }
}

/// Encodes a shared `tentaflow-sdk-spec` ABI input struct to CBOR for the
/// host call.
fn encode_cbor_input<T: minicbor::Encode<()>>(value: &T) -> Result<Vec<u8>, AbiError> {
    let mut buf = Vec::new();
    minicbor::encode(value, &mut buf).map_err(|_| AbiError::Operation)?;
    Ok(buf)
}

fn camera_add_payload(spec: &CameraAddSpec) -> tentaflow_sdk_spec::CameraAddInput {
    let (resolution_width, resolution_height) = match spec.resolution {
        Some((w, h)) => (Some(w), Some(h)),
        None => (None, None),
    };
    tentaflow_sdk_spec::CameraAddInput {
        display_name: spec.display_name.clone(),
        vendor: spec.vendor.clone(),
        url: spec.url.clone(),
        target_fps: Some(spec.target_fps),
        analysis_fps: Some(spec.analysis_fps),
        resolution_width,
        resolution_height,
        retention_class: Some(spec.retention_class.clone()),
        profile: Some(spec.profile.clone()),
        credentials_b64: spec.credentials_b64.clone(),
        onvif_profile_token: spec.onvif_profile_token.clone(),
    }
}

/// Rejestruje nowa kamere w supervisor + DB. F1a vendor whitelist: `fake_file`.
pub fn camera_add(spec: &CameraAddSpec) -> Result<CameraAddResult, AbiError> {
    let payload = encode_cbor_input(&camera_add_payload(spec))?;
    let bytes = call_sql_with_one_input(camera_add_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraAddOutput = decode_cbor(&bytes)?;
    Ok(CameraAddResult {
        camera_id: out.camera_id,
        status: out.status,
    })
}

/// Zwraca wszystkie kamery nalezace do wywolujacego addona. Kazdy wpis zawiera
/// runtime metryki (`fps_actual`, `status`) z supervisora gdy session jest
/// aktywna; w przeciwnym razie wartosci z DB (po restarcie hosta).
pub fn camera_list() -> Result<Vec<CameraInfo>, AbiError> {
    let bytes = call_host_no_input(camera_list_v1)?;
    let resp: tentaflow_sdk_spec::CameraListOut = decode_cbor(&bytes)?;
    Ok(resp.camera.into_iter().map(CameraInfo::from).collect())
}

/// Lists the flows assignable as a camera's analysis flow (`(id, name)`), for
/// the per-camera flow selector. Read-only; needs `cameras.read`.
pub fn camera_analysis_flows() -> Result<Vec<CameraAnalysisFlow>, AbiError> {
    let bytes = call_host_no_input(camera_analysis_flows_list_v1)?;
    let resp: tentaflow_sdk_spec::CameraAnalysisFlowsOut = decode_cbor(&bytes)?;
    Ok(resp
        .flows
        .into_iter()
        .map(|f| CameraAnalysisFlow {
            id: f.id,
            name: f.name,
        })
        .collect())
}

/// Lists every camera CV pipeline (summaries only — the JSON body is fetched
/// per-pipeline via [`camera_cv_pipeline_get`]). Read-only; needs `cameras.read`.
pub fn camera_cv_pipelines_list() -> Result<Vec<CameraCvPipelineSummary>, AbiError> {
    let bytes = call_host_no_input(camera_cv_pipelines_list_v1)?;
    let resp: tentaflow_sdk_spec::CameraCvPipelinesOut = decode_cbor(&bytes)?;
    Ok(resp
        .pipelines
        .into_iter()
        .map(|p| CameraCvPipelineSummary {
            id: p.id,
            name: p.name,
            is_default: p.is_default,
            updated_at: p.updated_at,
        })
        .collect())
}

/// Fetches one pipeline with its full JSON body. Needs `cameras.read`.
pub fn camera_cv_pipeline_get(id: &str) -> Result<CameraCvPipeline, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraCvPipelineIdInput {
        id: id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(camera_cv_pipeline_get_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraCvPipelineOut = decode_cbor(&bytes)?;
    Ok(CameraCvPipeline {
        id: out.id,
        name: out.name,
        pipeline_json: out.pipeline_json,
    })
}

/// Creates (`id = None` → host mints a fresh uuid) or updates a pipeline.
/// The host validates structure + model-alias existence; a rejected pipeline
/// returns `Ok` with the readable error in the result. Needs `cameras.write`.
pub fn camera_cv_pipeline_save(
    id: Option<&str>,
    name: &str,
    pipeline_json: &str,
) -> Result<CameraCvPipelineSaveResult, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraCvPipelineSaveInput {
        id: id.map(|s| s.to_string()),
        name: name.to_string(),
        pipeline_json: pipeline_json.to_string(),
    })?;
    let bytes = call_sql_with_one_input(camera_cv_pipeline_save_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraCvPipelineSaveOut = decode_cbor(&bytes)?;
    Ok(CameraCvPipelineSaveResult {
        id: out.id,
        error: out.error,
    })
}

/// Deletes a pipeline. Refusals (default pipeline, still assigned to a
/// camera) return `Ok` with `deleted = false` + the readable reason.
/// Needs `cameras.write`.
pub fn camera_cv_pipeline_delete(id: &str) -> Result<CameraCvPipelineDeleteResult, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraCvPipelineIdInput {
        id: id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(camera_cv_pipeline_delete_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraCvPipelineDeleteOut = decode_cbor(&bytes)?;
    Ok(CameraCvPipelineDeleteResult {
        deleted: out.deleted,
        error: out.error,
    })
}

/// Pobiera pojedynczy `CameraInfo`. Zwraca `NotFound` gdy kamera nie istnieje
/// lub nalezy do innego addona (kanalu bocznego nie ma — nie da sie wnioskowac
/// o istnieniu cudzych camera_id).
pub fn camera_get(camera_id: &str) -> Result<CameraInfo, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraIdInput {
        camera_id: camera_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(camera_get_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraInfoOut = decode_cbor(&bytes)?;
    Ok(CameraInfo::from(out))
}

/// Patch on-the-fly. Vendor + URL sa niezmienne — change them by remove + add.
pub fn camera_update(spec: &CameraUpdateSpec) -> Result<CameraInfo, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraUpdateInput {
        camera_id: spec.camera_id.clone(),
        display_name: spec.display_name.clone(),
        target_fps: spec.target_fps,
        analysis_fps: spec.analysis_fps,
        resolution_width: spec.resolution_width,
        resolution_height: spec.resolution_height,
        retention_class: spec.retention_class.clone(),
        profile: spec.profile.clone(),
        analysis_flow_id: spec.analysis_flow_id.clone(),
        cv_pipeline_id: spec.cv_pipeline_id.clone(),
    })?;
    let bytes = call_sql_with_one_input(camera_update_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraInfoOut = decode_cbor(&bytes)?;
    Ok(CameraInfo::from(out))
}

/// Soft-delete (stamps `removed_at`). Idempotent w sensie ABI: druga proba na
/// tym samym camera_id zwraca `NotFound`.
pub fn camera_remove(camera_id: &str) -> Result<(), AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraIdInput {
        camera_id: camera_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(camera_remove_v1, &payload)?;
    let _: tentaflow_sdk_spec::CameraRemoveOut = decode_cbor(&bytes)?;
    Ok(())
}

/// Snapshot ostatniej ramki — RGB24 zdekodowany z base64. Maks ~5.5MB raw
/// (1280x720 mieci sie w PayloadKind::ServiceCall; 1920x1080 przekroczy limit
/// i zwroci `PayloadTooLarge`).
///
/// Requires TentaFlow core built with `--features camera`. Without it
/// addon instantiation fails at module-link time with "missing import".
pub fn camera_snapshot(camera_id: &str) -> Result<SnapshotInfo, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraIdInput {
        camera_id: camera_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input_capped(
        camera_snapshot_v1,
        &payload,
        MAX_OUT_CAP_SNAPSHOT,
    )?;
    let raw: tentaflow_sdk_spec::CameraSnapshotOut = decode_cbor(&bytes)?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(raw.data_b64.as_bytes())
        .map_err(|_| AbiError::Operation)?;
    Ok(SnapshotInfo {
        camera_id: raw.camera_id,
        width: raw.width,
        height: raw.height,
        pixel_format: raw.pixel_format,
        timestamp_unix_ms: raw.timestamp_unix_ms,
        data,
    })
}

/// Resize obrazu RGB24 (row-major, 3 bajty/piksel) z `src_w x src_h` do
/// `dst_w x dst_h`. Zwraca nowy bufor RGB24 `dst_w * dst_h * 3`. Host uzywa
/// najszybszego separowalnego resizera SIMD (AVX2 / NEON).
///
/// Rozmiar wyniku jest znany z gory (`dst_w*dst_h*3`), wiec bufor alokujemy
/// dokladnie — kod 6 (OutputBufferTooSmall) obslugujemy mimo to jednym retry
/// z rozmiarem zwroconym przez host (defensywnie).
///
/// Wymaga uprawnienia "image.resize" w manifescie addonu.
pub fn image_resize_rgb(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Result<Vec<u8>, AbiError> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Err(AbiError::Operation);
    }
    let expected_src = src_w as usize * src_h as usize * 3;
    if src.len() != expected_src {
        return Err(AbiError::Operation);
    }

    let mut cap = dst_w as usize * dst_h as usize * 3;
    if cap > MAX_IMAGE_OUT_CAP {
        return Err(AbiError::PayloadTooLarge);
    }

    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let mut buffer = vec![0u8; cap];
        let mut out_len: u32 = 0;
        let rc = unsafe {
            image_resize_rgb_v1(
                src.as_ptr() as i32,
                src.len() as i32,
                src_w as i32,
                src_h as i32,
                dst_w as i32,
                dst_h as i32,
                buffer.as_mut_ptr() as i32,
                cap as i32,
                &mut out_len as *mut u32 as i32,
            )
        };
        if rc == 0 {
            buffer.truncate(out_len as usize);
            return Ok(buffer);
        }
        if rc == AbiError::OutputBufferTooSmall.as_i32() {
            if attempts > MAX_RETRY_ATTEMPTS {
                return Err(AbiError::OutputBufferTooSmall);
            }
            let required = out_len as usize;
            if required <= cap {
                return Err(AbiError::OutputBufferTooSmall);
            }
            if required > MAX_IMAGE_OUT_CAP {
                return Err(AbiError::PayloadTooLarge);
            }
            cap = required;
            continue;
        }
        return Err(AbiError::from_i32(rc));
    }
}

/// Health + runtime metryki z supervisora. Gdy session zniknal (np. restart
/// hosta przed Issue #8 fix), zwraca `status_message = "session missing"` +
/// metryki = 0.
pub fn camera_health(camera_id: &str) -> Result<CameraHealthInfo, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraIdInput {
        camera_id: camera_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(camera_health_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraHealthOut = decode_cbor(&bytes)?;
    Ok(CameraHealthInfo {
        camera_id: out.camera_id,
        status: out.status,
        status_message: out.status_message,
        fps_actual: out.fps_actual,
        last_frame_at: out.last_frame_at,
        frames_total: out.frames_total,
        frames_dropped: out.frames_dropped,
    })
}

/// WS-Discovery na lokalnej sieci LAN — zwraca wykryte urzadzenia ONVIF
/// zmapowane na `CameraInfo` (jeszcze bez `camera_id`).
pub fn camera_discover() -> Result<Vec<CameraInfo>, AbiError> {
    let bytes = call_host_no_input(camera_discover_v1)?;
    let resp: tentaflow_sdk_spec::CameraDiscoverOut = decode_cbor(&bytes)?;
    Ok(resp
        .discovered
        .into_iter()
        .map(|d| CameraInfo {
            camera_id: String::new(),
            display_name: d.model.clone(),
            vendor: "onvif".to_string(),
            url: d.xaddrs.first().cloned().unwrap_or_default(),
            target_fps: 0,
            resolution_width: None,
            resolution_height: None,
            status: "discovered".to_string(),
            status_message: Some(format!("{} {}", d.manufacturer, d.model)),
            fps_actual: None,
            last_frame_at: None,
            retention_class: String::new(),
            profile: String::new(),
            analysis_flow_id: None,
            cv_pipeline_id: None,
        })
        .collect())
}

/// Lightweight probe — sprawdza czy URL kamery jest osiagalny dla danego
/// vendora. Dla `fake_file` sprawdza ze plik istnieje, jest plikiem regularnym
/// i nie zawiera symlinkow w sciezce.
pub fn camera_test_connection(vendor: &str, url: &str) -> Result<CameraTestResult, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraTestConnectionInput {
        vendor: vendor.to_string(),
        url: url.to_string(),
    })?;
    let bytes = call_sql_with_one_input(camera_test_connection_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraTestConnectionOut = decode_cbor(&bytes)?;
    Ok(CameraTestResult {
        ok: out.ok,
        message: out.message,
    })
}

/// Rotacja credentiali dla vendorow wymagajacych auth (RTSP/ONVIF).
/// `new_credentials_b64 = None` czysci credential. Zwraca `(rotated, reason)`.
pub fn camera_credentials_rotate(
    camera_id: &str,
    new_credentials_b64: Option<&str>,
) -> Result<(bool, String), AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraCredentialsRotateInput {
        camera_id: camera_id.to_string(),
        new_credentials_b64: new_credentials_b64.map(|s| s.to_string()),
    })?;
    let bytes = call_sql_with_one_input(camera_credentials_rotate_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraCredentialsRotateOut = decode_cbor(&bytes)?;
    Ok((out.rotated, out.reason))
}

// =============================================================================
// Streaming API wrappers (F1a M1.W7) — `stream_subscribe / next / close`.
//
// **All `stream_*` wrappers require TentaFlow core built with
// `--features camera`.** Without it the host functions are not registered and
// module instantiation fails at link time with "missing import".
// =============================================================================

/// Payload metadata for a Frame message returned by `stream_next`. Bytes live
/// in the core LRU and travel to a service via `service_call` + PickupToken —
/// the addon never receives them inline.
#[derive(Debug, Clone)]
pub struct StreamFrameMeta {
    pub frame_ref: String,
    pub camera_id: String,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub timestamp_unix_ms: u64,
}

/// Message variants the addon can observe on a subscribed stream.
#[derive(Debug, Clone)]
pub enum StreamNextMessage {
    Frame(StreamFrameMeta),
    Drop { count: u64 },
    CameraOffline { reason: String },
    StreamClosed,
    Timeout,
}

/// Subscribe to a camera's frame bus. F1a target format: `camera:<camera_id>`.
/// Ownership is enforced — addons cannot subscribe to cameras owned by other
/// addons (returns `NotFound`).
pub fn stream_subscribe(target: &str, max_fps: Option<u32>) -> Result<String, AbiError> {
    let input = tentaflow_sdk_spec::StreamSubscribeInput {
        target: target.to_string(),
        filter: max_fps.map(|fps| tentaflow_sdk_spec::StreamSubscribeFilter {
            max_fps: Some(fps),
            skip_frames: Some(0),
        }),
    };
    let payload = encode_cbor_input(&input)?;
    let bytes = call_sql_with_one_input_capped(stream_subscribe_v1, &payload, MAX_OUT_CAP_STREAM)?;
    let out: tentaflow_sdk_spec::StreamSubscribeOutput = decode_cbor(&bytes)?;
    Ok(out.stream_id)
}

/// Bounded-await poll for the next stream message. `timeout_ms` is clamped to
/// 5000 ms by the host.
pub fn stream_next(stream_id: &str, timeout_ms: u64) -> Result<StreamNextMessage, AbiError> {
    let input = tentaflow_sdk_spec::StreamNextInput {
        stream_id: stream_id.to_string(),
        timeout_ms,
    };
    let payload = encode_cbor_input(&input)?;
    let bytes = call_sql_with_one_input_capped(stream_next_v1, &payload, MAX_OUT_CAP_STREAM)?;
    let out: tentaflow_sdk_spec::StreamNextOutput = decode_cbor(&bytes)?;
    // Map the host's tagged output onto the SDK enum. A missing field for the
    // declared `kind` means the host produced a malformed frame, which we
    // surface as `Operation` rather than silently dropping it.
    match out.kind.as_str() {
        "frame" => Ok(StreamNextMessage::Frame(StreamFrameMeta {
            frame_ref: out.frame_ref.ok_or(AbiError::Operation)?,
            camera_id: out.camera_id.ok_or(AbiError::Operation)?,
            width: out.width.ok_or(AbiError::Operation)?,
            height: out.height.ok_or(AbiError::Operation)?,
            pixel_format: out.pixel_format.ok_or(AbiError::Operation)?,
            timestamp_unix_ms: out.timestamp_unix_ms.ok_or(AbiError::Operation)?,
        })),
        "drop" => Ok(StreamNextMessage::Drop {
            count: out.count.ok_or(AbiError::Operation)?,
        }),
        "camera_offline" => Ok(StreamNextMessage::CameraOffline {
            reason: out.reason.ok_or(AbiError::Operation)?,
        }),
        "stream_closed" => Ok(StreamNextMessage::StreamClosed),
        "timeout" => Ok(StreamNextMessage::Timeout),
        _ => Err(AbiError::Operation),
    }
}

/// Drop the subscription. Subsequent `stream_next` calls for the same id
/// return `StreamNotFound`.
pub fn stream_close(stream_id: &str) -> Result<(), AbiError> {
    let input = tentaflow_sdk_spec::StreamCloseInput {
        stream_id: stream_id.to_string(),
    };
    let payload = encode_cbor_input(&input)?;
    let bytes = call_sql_with_one_input_capped(stream_close_v1, &payload, MAX_OUT_CAP_STREAM)?;
    let _: tentaflow_sdk_spec::StreamCloseOutput = decode_cbor(&bytes)?;
    Ok(())
}

// =============================================================================
// Camera metadata API wrappers (F2 P6.b) — ONVIF analytics events.
//
// Subscribe to a camera that already has `metadata_supported = true` (set
// when ONVIF discovery found a non-empty Media2 metadata configuration).
// Each `camera_metadata_poll` returns up to `max_items` analytics frames
// pulled from the camera's PullPoint stream, plus backpressure / offline
// signals. The host fn enforces `camera.metadata` permission, org isolation
// and the metadata_supported gate.
//
// Requires TentaFlow core built with `--features camera`.
// =============================================================================

/// One detected object inside a `MetadataFrame`. Fields mirror the ONVIF
/// analytics schema: `class` (e.g. "Vehicle"), confidence in 0..1,
/// optional `bbox` as `[left, top, right, bottom]` in normalised 0..1
/// device coordinates, and an optional `track_id` for cross-frame
/// correlation.
#[derive(Debug, Clone, Deserialize)]
pub struct MetadataItem {
    pub class: String,
    pub confidence: f64,
    #[serde(default)]
    pub bbox: Option<[f64; 4]>,
    #[serde(default)]
    pub track_id: Option<String>,
}

/// One analytics frame — a single device tick that produced one or more
/// detections.
#[derive(Debug, Clone, Deserialize)]
pub struct MetadataFrame {
    pub camera_id: String,
    /// Event timestamp in unix milliseconds (as supplied by the device,
    /// times 1000). May be 0 if the camera omitted the `UtcTime` attribute.
    pub ts_unix_ms: i64,
    pub items: Vec<MetadataItem>,
}

/// Aggregate poll outcome — frames plus optional backpressure / offline
/// signals.
#[derive(Debug, Clone)]
pub struct MetadataPollResult {
    pub frames: Vec<MetadataFrame>,
    /// True iff the camera went offline mid-poll (the supervisor task
    /// exited or `CameraOffline` was raised on the bus). The addon should
    /// stop polling this subscription.
    pub camera_offline: bool,
    /// Number of frames the host dropped due to bus backpressure since the
    /// last successful poll.
    pub dropped: u64,
}

/// Subscribe to a camera's ONVIF analytics-metadata stream. The host spawns
/// (or refcounts) a per-camera PullPoint task; the first subscriber pays
/// the cost of `CreatePullPointSubscription` against the camera. Errors:
///
///   * `Permission` — addon lacks `camera.metadata` or the camera lives in
///                    another org.
///   * `NotFound`   — camera does not exist (or is not owned by this addon).
///   * `Operation`  — camera does not advertise metadata
///                    (`metadata_supported = false`), or ONVIF credentials
///                    are missing.
///   * `CameraUnreachable` — transport error talking to the events service.
pub fn camera_metadata_subscribe(camera_id: &str) -> Result<String, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::MetadataSubscribeInput {
        camera_id: camera_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input_capped(
        camera_metadata_subscribe_v1,
        &payload,
        MAX_OUT_CAP_STREAM,
    )?;
    let out: tentaflow_sdk_spec::MetadataSubscribeOutput = decode_cbor(&bytes)?;
    Ok(out.subscription_id)
}

/// Bounded-await poll for the next batch of analytics frames. `timeout_ms`
/// is clamped to 30 000 ms host-side; `max_items` is clamped to 100.
pub fn camera_metadata_poll(
    subscription_id: &str,
    max_items: u32,
    timeout_ms: u32,
) -> Result<MetadataPollResult, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::MetadataPollInput {
        subscription_id: subscription_id.to_string(),
        max_items: Some(max_items),
        timeout_ms: Some(timeout_ms),
    })?;
    let bytes =
        call_sql_with_one_input_capped(camera_metadata_poll_v1, &payload, MAX_OUT_CAP_STREAM)?;
    let raw: tentaflow_sdk_spec::MetadataPollOutput = decode_cbor(&bytes)?;
    Ok(MetadataPollResult {
        frames: raw
            .frames
            .into_iter()
            .map(|f| MetadataFrame {
                camera_id: f.camera_id,
                ts_unix_ms: f.ts_unix_ms,
                items: f
                    .items
                    .into_iter()
                    .map(|i| MetadataItem {
                        class: i.class,
                        confidence: i.confidence,
                        bbox: i.bbox,
                        track_id: i.track_id,
                    })
                    .collect(),
            })
            .collect(),
        camera_offline: raw.camera_offline,
        dropped: raw.dropped,
    })
}

/// Drop the subscription. Idempotent: a second call for the same id
/// (or one for an unknown id) returns `Ok(false)`. The supervisor pull
/// task is cancelled when the last addon unsubscribes from the camera.
pub fn camera_metadata_unsubscribe(subscription_id: &str) -> Result<bool, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::MetadataUnsubscribeInput {
        subscription_id: subscription_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input_capped(
        camera_metadata_unsubscribe_v1,
        &payload,
        MAX_OUT_CAP_STREAM,
    )?;
    let out: tentaflow_sdk_spec::MetadataUnsubscribeOutput = decode_cbor(&bytes)?;
    Ok(out.unsubscribed)
}

// =============================================================================
// Recording API wrappers (F1a M1.W8) — snapshots, segments, signed URLs.
//
// All wrappers require TentaFlow core built with `--features camera`.
// =============================================================================

/// Metadata for a recording artifact persisted on the host (PNG snapshot or
/// MP4 segment). `recording_ref` is the public handle (`snap_<uuid>` /
/// `clip_<uuid>`) used by the other recording APIs.
#[derive(Debug, Clone)]
pub struct SavedRecordingInfo {
    pub recording_ref: String,
    pub file_path: String,
    pub file_size_bytes: u64,
    pub duration_ms: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub hash_sha256: String,
    pub created_at: u64,
}

/// Signed URL for a stored recording or a raw frame. Multi-use until expiry.
#[derive(Debug, Clone)]
pub struct RecordingUrl {
    pub url: String,
    pub expires_unix_ms: u64,
}

/// Signed URL for a raw frame in the LRU. Shape mirrors `RecordingUrl` so the
/// SDK surface stays symmetric; lives as its own type for self-documenting
/// call sites.
#[derive(Debug, Clone)]
pub struct FrameUrl {
    pub url: String,
    pub expires_unix_ms: u64,
}

#[derive(Debug, Clone)]
pub struct RecordingStatsPerCamera {
    pub camera_id: String,
    pub snapshots: u64,
    pub segments: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct RecordingStats {
    pub total_snapshots: u64,
    pub total_segments: u64,
    pub total_size_bytes: u64,
    pub per_camera: Vec<RecordingStatsPerCamera>,
}

/// Inline raw bytes of a stored recording plus integrity metadata so the addon
/// can verify the payload against the host's SHA-256 hash before consuming it.
#[derive(Debug, Clone)]
pub struct RecordingStream {
    pub bytes: Vec<u8>,
    pub file_size_bytes: u64,
    pub hash_sha256: String,
}

fn save_recording_info_from(out: tentaflow_sdk_spec::SaveRecordingOut) -> SavedRecordingInfo {
    SavedRecordingInfo {
        recording_ref: out.recording_ref,
        file_path: out.file_path,
        file_size_bytes: out.file_size_bytes,
        duration_ms: out.duration_ms,
        width: out.width,
        height: out.height,
        hash_sha256: out.hash_sha256,
        created_at: out.created_at,
    }
}

/// Persist a PNG snapshot for a frame already living in the host's LRU.
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_save_snapshot(
    camera_id: &str,
    frame_ref: &str,
    retention_class: Option<&str>,
) -> Result<SavedRecordingInfo, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::RecordingSaveSnapshotInput {
        camera_id: camera_id.to_string(),
        frame_ref: frame_ref.to_string(),
        retention_class: retention_class.map(|s| s.to_string()),
    })?;
    let bytes = call_sql_with_one_input(recording_save_snapshot_v1, &payload)?;
    let out: tentaflow_sdk_spec::SaveRecordingOut = decode_cbor(&bytes)?;
    Ok(save_recording_info_from(out))
}

/// Capture `duration_secs` of the camera's bound source into an MP4 segment.
/// The source is always derived host-side from the owning camera row — addons
/// cannot supply an arbitrary `source_url`. F1a accepts only cameras with
/// `vendor='fake_file'`.
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_save_segment(
    camera_id: &str,
    duration_secs: u32,
    retention_class: Option<&str>,
) -> Result<SavedRecordingInfo, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::RecordingSaveSegmentInput {
        camera_id: camera_id.to_string(),
        duration_secs,
        retention_class: retention_class.map(|s| s.to_string()),
    })?;
    let bytes = call_sql_with_one_input(recording_save_segment_v1, &payload)?;
    let out: tentaflow_sdk_spec::SaveRecordingOut = decode_cbor(&bytes)?;
    Ok(save_recording_info_from(out))
}

/// Issue a multi-use signed URL for a stored recording. TTL must be in
/// `60..=3600` seconds.
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_get_url(recording_ref: &str, ttl_secs: u64) -> Result<RecordingUrl, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::RecordingGetUrlInput {
        recording_ref: recording_ref.to_string(),
        ttl_secs,
    })?;
    let bytes = call_sql_with_one_input(recording_get_url_v1, &payload)?;
    let out: tentaflow_sdk_spec::UrlOut = decode_cbor(&bytes)?;
    Ok(RecordingUrl {
        url: out.url,
        expires_unix_ms: out.expires_unix_ms,
    })
}

/// Fetch the raw bytes (PNG or MP4) of a stored recording inline together with
/// the host's reported size and SHA-256 hash. The CBOR envelope is hard-capped
/// at 8 MiB; after base64 expansion this admits files up to ~6 MiB raw. Larger
/// artifacts must be fetched via the signed URL + HTTP handler.
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_get_stream(recording_ref: &str) -> Result<RecordingStream, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::RecordingRefInput {
        recording_ref: recording_ref.to_string(),
    })?;
    let bytes =
        call_sql_with_one_input_capped(recording_get_stream_v1, &payload, MAX_OUT_CAP_SNAPSHOT)?;
    let raw: tentaflow_sdk_spec::GetStreamOut = decode_cbor(&bytes)?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(raw.data_b64.as_bytes())
        .map_err(|_| AbiError::Operation)?;
    Ok(RecordingStream {
        bytes: decoded,
        file_size_bytes: raw.file_size_bytes,
        hash_sha256: raw.hash_sha256,
    })
}

/// Soft-delete + filesystem purge. Idempotent: a second call on the same ref
/// returns `NotFound`.
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_purge(recording_ref: &str) -> Result<(), AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::RecordingRefInput {
        recording_ref: recording_ref.to_string(),
    })?;
    let bytes = call_sql_with_one_input(recording_purge_v1, &payload)?;
    let _: tentaflow_sdk_spec::PurgeOut = decode_cbor(&bytes)?;
    Ok(())
}

/// Aggregate recording counts + size per addon (optionally narrowed to a
/// single camera).
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_stats(camera_id: Option<&str>) -> Result<RecordingStats, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::RecordingStatsInput {
        camera_id: camera_id.map(|s| s.to_string()),
    })?;
    let bytes = call_sql_with_one_input(recording_stats_v1, &payload)?;
    let raw: tentaflow_sdk_spec::StatsOut = decode_cbor(&bytes)?;
    Ok(RecordingStats {
        total_snapshots: raw.stats.total_snapshots,
        total_segments: raw.stats.total_segments,
        total_size_bytes: raw.stats.total_size_bytes,
        per_camera: raw
            .per_camera
            .into_iter()
            .map(|c| RecordingStatsPerCamera {
                camera_id: c.camera_id,
                snapshots: c.snapshots,
                segments: c.segments,
                size_bytes: c.size_bytes,
            })
            .collect(),
    })
}

/// Issue a multi-use signed URL for a raw frame in the host LRU. TTL must be
/// in `60..=600` seconds. Frame must belong to a camera owned by the calling
/// addon.
/// Requires TentaFlow core built with `--features camera`.
pub fn frame_url(frame_ref: &str, ttl_secs: u64) -> Result<FrameUrl, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::FrameUrlInput {
        frame_ref: frame_ref.to_string(),
        ttl_secs,
    })?;
    let bytes = call_sql_with_one_input(frame_url_v1, &payload)?;
    let out: tentaflow_sdk_spec::UrlOut = decode_cbor(&bytes)?;
    Ok(FrameUrl {
        url: out.url,
        expires_unix_ms: out.expires_unix_ms,
    })
}

// =============================================================================
// Vector API wrappers (F1c P3) — embedded HNSW per-namespace storage
// =============================================================================

/// Backend-agnostic vector metadata + filter types, re-exported under `Vector*`
/// names so addons build typed fields and filters "our way" without depending
/// on `tentaflow-sdk-spec` directly. The core translates a [`VectorFilter`] to
/// the selected backend (zvec / Milvus).
pub use tentaflow_sdk_spec::{
    Field as VectorField, FieldType as VectorFieldType, FieldValue as VectorFieldValue,
    Filter as VectorFilter, Fusion as VectorFusion, SparseVector,
};

/// One hit returned by `vector_search`. `ref_id` is the key the addon supplied
/// during `vector_upsert`; `score` is the raw metric distance (lower = closer
/// for cosine/euclidean; `1 - dot` for dot). `fields` carries the metadata
/// values requested via `output_fields` (empty when none requested).
#[derive(Debug, Clone)]
pub struct VectorHit {
    pub ref_id: u64,
    pub score: f32,
    pub fields: Vec<tentaflow_sdk_spec::Field>,
}

/// Encode a `&[f32]` slice as base64(little-endian f32 bytes) for the vector
/// host functions. Exposed publicly so addons can pre-encode embeddings once
/// and reuse the string across upsert/search calls.
pub fn encode_vector_b64(vector: &[f32]) -> String {
    let mut raw = Vec::with_capacity(vector.len() * 4);
    for f in vector {
        raw.extend_from_slice(&f.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(&raw)
}

/// Insert or replace a vector under `ref_id` in `namespace`, with optional
/// typed metadata `fields`. Returns the total vector count after the upsert.
/// Requires `vector.write` permission and the namespace must be declared in the
/// addon manifest under `[[vector_namespace]]`; every field's name + type must
/// match that namespace's declared `fields` schema. Pass `&[]` for no metadata.
pub fn vector_upsert(
    namespace: &str,
    ref_id: u64,
    vector: &[f32],
    fields: &[tentaflow_sdk_spec::Field],
) -> Result<u64, AbiError> {
    vector_upsert_sparse(namespace, ref_id, vector, fields, None)
}

/// Like [`vector_upsert`] but also stores a sparse vector for hybrid search.
/// Only valid when the namespace declares `sparse = true` in the manifest.
pub fn vector_upsert_sparse(
    namespace: &str,
    ref_id: u64,
    vector: &[f32],
    fields: &[tentaflow_sdk_spec::Field],
    sparse: Option<&SparseVector>,
) -> Result<u64, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::VectorUpsertInput {
        namespace: namespace.to_string(),
        ref_id,
        vector_b64: encode_vector_b64(vector),
        fields: (!fields.is_empty()).then(|| fields.to_vec()),
        sparse: sparse.cloned(),
    })?;
    let bytes = call_sql_with_one_input(vector_upsert_v1, &payload)?;
    let resp: tentaflow_sdk_spec::VectorUpsertOutput = decode_cbor(&bytes)?;
    Ok(resp.count)
}

/// Hybrid dense + sparse k-NN over a namespace declared with `sparse = true`.
/// `fusion = None` uses RRF (rank constant 60), the robust default for RAG.
/// `filter` and `output_fields` behave as in [`vector_search`].
#[allow(clippy::too_many_arguments)]
pub fn vector_hybrid_search(
    namespace: &str,
    dense: &[f32],
    sparse: &SparseVector,
    k: u32,
    gate_claim_id: Option<&str>,
    filter: Option<&tentaflow_sdk_spec::Filter>,
    output_fields: &[&str],
    fusion: Option<VectorFusion>,
) -> Result<Vec<VectorHit>, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::VectorHybridSearchInput {
        namespace: namespace.to_string(),
        dense_b64: encode_vector_b64(dense),
        sparse: sparse.clone(),
        k,
        gate_claim_id: gate_claim_id.map(str::to_string),
        filter: filter.cloned(),
        output_fields: (!output_fields.is_empty())
            .then(|| output_fields.iter().map(|s| s.to_string()).collect()),
        fusion,
    })?;
    let bytes = call_sql_with_one_input(vector_hybrid_search_v1, &payload)?;
    let resp: tentaflow_sdk_spec::VectorSearchOutput = decode_cbor(&bytes)?;
    Ok(resp
        .hits
        .into_iter()
        .map(|h| VectorHit {
            ref_id: h.ref_id,
            score: h.score,
            fields: h.fields.unwrap_or_default(),
        })
        .collect())
}

/// Top-k k-NN search. Pass `gate_claim_id = Some(...)` when the namespace
/// declares a `gate` in the manifest (P4 policy/claims engine validates the
/// claim; P3 only enforces the structural presence). `filter` restricts results
/// by metadata using the backend-agnostic [`tentaflow_sdk_spec::Filter`] AST
/// (the core translates it to the selected backend); `output_fields` lists the
/// declared metadata fields to return on each hit (empty = ref_id + score only).
pub fn vector_search(
    namespace: &str,
    query: &[f32],
    k: u32,
    gate_claim_id: Option<&str>,
    filter: Option<&tentaflow_sdk_spec::Filter>,
    output_fields: &[&str],
) -> Result<Vec<VectorHit>, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::VectorSearchInput {
        namespace: namespace.to_string(),
        query_b64: encode_vector_b64(query),
        k,
        gate_claim_id: gate_claim_id.map(str::to_string),
        filter: filter.cloned(),
        output_fields: (!output_fields.is_empty())
            .then(|| output_fields.iter().map(|s| s.to_string()).collect()),
    })?;
    let bytes = call_sql_with_one_input(vector_search_v1, &payload)?;
    let resp: tentaflow_sdk_spec::VectorSearchOutput = decode_cbor(&bytes)?;
    Ok(resp
        .hits
        .into_iter()
        .map(|h| VectorHit {
            ref_id: h.ref_id,
            score: h.score,
            fields: h.fields.unwrap_or_default(),
        })
        .collect())
}

/// Remove the vector under `ref_id`. Returns `true` if the key existed.
pub fn vector_delete(namespace: &str, ref_id: u64) -> Result<bool, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::VectorDeleteInput {
        namespace: namespace.to_string(),
        ref_id,
    })?;
    let bytes = call_sql_with_one_input(vector_delete_v1, &payload)?;
    let resp: tentaflow_sdk_spec::VectorDeleteOutput = decode_cbor(&bytes)?;
    Ok(resp.removed)
}

// =============================================================================
// Document parse API wrapper (RAG E1.2)
// =============================================================================

pub use tentaflow_sdk_spec::{DocBlock, DocParseOutput};

/// Parsuje OBRAZ strony dokumentu (`image` — surowe bajty PNG/JPEG, `mime` —
/// ich typ) na markdown + bloki layoutu przez serwis vision-parse. `model_alias`
/// = `None` → domyślny alias `rag-parse` (alias-aware failover jak reranker).
/// Wymaga `document.parse`. Zwraca pełny markdown strony, bloki ([`DocBlock`])
/// i `page_count` (zawsze 1 dla pojedynczego obrazu).
pub fn doc_parse(
    image: &[u8],
    mime: &str,
    model_alias: Option<&str>,
) -> Result<DocParseOutput, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::DocParseInput {
        image_b64: base64::engine::general_purpose::STANDARD.encode(image),
        mime: mime.to_string(),
        model_alias: model_alias.map(str::to_string),
    })?;
    let bytes = call_sql_with_one_input(doc_parse_v1, &payload)?;
    decode_cbor(&bytes)
}

// =============================================================================
// Ingest-as-flow API wrapper (RAG Partia 3)
// =============================================================================

pub use tentaflow_sdk_spec::IngestInvokeOutput;

/// Uruchamia flow-ingest JEDNEGO dokumentu z BINARNYM payloadem. `doc_id_blob`
/// to id pliku w per-instance document store (zwrócone przez upload /
/// [`document_put`]); host pobiera bajty po swojej stronie (zero podwójnego
/// transferu przez ABI), seeduje binarny envelope (`FlowValue::Image` dla obrazu,
/// `Other` dla PDF/xlsx/docx) i dispatchuje flow `<model>:ingest:document`.
/// `options` to opaque JSON (collection_id, graph toggle, parametry chunkingu)
/// wstrzyknięty do flow.meta. Wymaga `document.read`. Zwraca markdown
/// rekonstrukcji + liczbę zapisanych chunków ([`IngestInvokeOutput`]).
///
/// To JEDYNA ścieżka wywołania flow-ingestu z surowym dokumentem — `llm_generate`
/// buduje wyłącznie tekstową wiadomość.
pub fn ingest_invoke(
    doc_id_blob: &str,
    mime: &str,
    model: &str,
    options: Option<&str>,
) -> Result<IngestInvokeOutput, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::IngestInvokeInput {
        doc_id_blob: doc_id_blob.to_string(),
        mime: mime.to_string(),
        model: model.to_string(),
        options_json: options.map(str::to_string),
    })?;
    let bytes = call_sql_with_one_input(ingest_invoke_v1, &payload)?;
    decode_cbor(&bytes)
}

// =============================================================================
// Document/blob store API wrappers (RAG E1.3) — per-instance file upload store
// =============================================================================

pub use tentaflow_sdk_spec::{
    DocumentDeleteOutput, DocumentGetMeta, DocumentMeta, DocumentPutOutput,
};

/// Rozmiar kawałka uploadu używany przez [`document_put`]. Dobrany pod sufit
/// metadanych CBOR (bajty i tak jadą osobnym ptr/len) i deterministyczną
/// iterację po stronie hosta. Większy plik = więcej wywołań `document_put_v1`.
pub const DOCUMENT_PUT_CHUNK_BYTES: usize = 256 * 1024;

/// Wgrywa kompletny plik do per-instance document store, dzieląc go na kawałki
/// i wołając `document_put_v1` sekwencyjnie. `doc_id = None` → host generuje
/// nowy identyfikator (zwrócony w wyniku); `Some(id)` nadpisuje istniejący.
/// Zwraca finalny [`DocumentPutOutput`] (`finalized = true`, `sha256`, rozmiar).
/// Wymaga `document.write`. Plik > limitu KV 1 MB jest tu legalny — to właśnie
/// powód istnienia tego store'u.
pub fn document_put(
    doc_id: Option<&str>,
    mime: &str,
    data: &[u8],
) -> Result<DocumentPutOutput, AbiError> {
    // Pusty plik = jeden pusty kawałek (total_chunks = 1), żeby finalizacja i
    // tak nastąpiła i powstał wpis rejestru.
    let total_chunks = data.len().div_ceil(DOCUMENT_PUT_CHUNK_BYTES).max(1) as u32;
    // doc_id pierwszego kawałka: podany albo pusty (host wygeneruje); kolejne
    // kawałki MUSZĄ użyć już ustalonego id, więc trzymamy go między iteracjami.
    let mut current_id = doc_id.unwrap_or("").to_string();
    let mut last: Option<DocumentPutOutput> = None;
    for chunk_index in 0..total_chunks {
        let start = chunk_index as usize * DOCUMENT_PUT_CHUNK_BYTES;
        let end = (start + DOCUMENT_PUT_CHUNK_BYTES).min(data.len());
        let chunk = &data[start..end];
        let payload = encode_cbor_input(&tentaflow_sdk_spec::DocumentPutInput {
            doc_id: current_id.clone(),
            mime: mime.to_string(),
            chunk_index,
            total_chunks,
        })?;
        let bytes = call_document_put(&payload, chunk)?;
        let out: DocumentPutOutput = decode_cbor(&bytes)?;
        // Po pierwszym kawałku znamy ostateczny doc_id (host mógł go wygenerować).
        current_id = out.doc_id.clone();
        last = Some(out);
    }
    last.ok_or(AbiError::Operation)
}

/// Pobiera kompletny plik z document store, czytając kolejne kawałki przez
/// `document_get_v1` aż do `total_chunks`. Zwraca `(bajty, mime)`. Wymaga
/// `document.read`. Obcy/nieistniejący `doc_id` → `AbiError::NotFound`.
pub fn document_get(doc_id: &str) -> Result<(Vec<u8>, String), AbiError> {
    let mut assembled = Vec::new();
    let mut mime = String::new();
    let mut chunk_index: u32 = 0;
    loop {
        let payload = encode_cbor_input(&tentaflow_sdk_spec::DocumentGetInput {
            doc_id: doc_id.to_string(),
            chunk_index,
        })?;
        let (chunk, meta) = call_document_get(&payload)?;
        assembled.extend_from_slice(&chunk);
        mime = meta.mime;
        chunk_index += 1;
        if chunk_index >= meta.total_chunks {
            break;
        }
    }
    Ok((assembled, mime))
}

/// Kasuje dokument (plik + wpis rejestru) po `doc_id`. Idempotentny —
/// nieistniejący `doc_id` zwraca `removed = false`. Wymaga `document.write`.
pub fn document_delete(doc_id: &str) -> Result<DocumentDeleteOutput, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::DocumentDeleteInput {
        doc_id: doc_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(document_delete_v1, &payload)?;
    decode_cbor(&bytes)
}

/// Listuje metadane wszystkich dokumentów tej instancji. Wymaga `document.read`.
pub fn document_list() -> Result<Vec<DocumentMeta>, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::DocumentListInput {})?;
    let bytes = call_sql_with_one_input(document_list_v1, &payload)?;
    let out: tentaflow_sdk_spec::DocumentListOutput = decode_cbor(&bytes)?;
    Ok(out.documents)
}

/// Woła `document_put_v1` (metadane CBOR + osobny bufor bajtów kawałka) z retry
/// na za mały bufor wyjścia metadanych.
fn call_document_put(meta: &[u8], chunk: &[u8]) -> Result<Vec<u8>, AbiError> {
    let mut cap = INITIAL_CAP;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let mut buffer = vec![0u8; cap];
        let mut out_len: u32 = 0;
        let rc = unsafe {
            document_put_v1(
                meta.as_ptr() as i32,
                meta.len() as i32,
                chunk.as_ptr() as i32,
                chunk.len() as i32,
                buffer.as_mut_ptr() as i32,
                cap as i32,
                &mut out_len as *mut u32 as i32,
            )
        };
        if rc == 0 {
            buffer.truncate(out_len as usize);
            return Ok(buffer);
        }
        if rc == AbiError::OutputBufferTooSmall.as_i32() {
            let required = out_len as usize;
            if attempts > MAX_RETRY_ATTEMPTS || required <= cap {
                return Err(AbiError::OutputBufferTooSmall);
            }
            if required > MAX_OUT_CAP {
                return Err(AbiError::PayloadTooLarge);
            }
            cap = required;
            continue;
        }
        return Err(AbiError::from_i32(rc));
    }
}

/// Woła `document_get_v1`: bajty kawałka lądują w osobnym buforze, metadane w
/// drugim. Gdy bufor bajtów za mały, host zwraca wymagany rozmiar w
/// `meta_out_len_ptr` (retry semantics) i NIE pisze metadanych — realokujemy
/// bufor bajtów i ponawiamy. Zwraca `(bajty_kawałka, metadane)`.
fn call_document_get(meta_in: &[u8]) -> Result<(Vec<u8>, DocumentGetMeta), AbiError> {
    let mut blob_cap = DOCUMENT_PUT_CHUNK_BYTES;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let mut blob_buf = vec![0u8; blob_cap];
        let mut meta_buf = vec![0u8; INITIAL_CAP];
        let mut out_len: u32 = 0;
        let rc = unsafe {
            document_get_v1(
                meta_in.as_ptr() as i32,
                meta_in.len() as i32,
                blob_buf.as_mut_ptr() as i32,
                blob_cap as i32,
                meta_buf.as_mut_ptr() as i32,
                meta_buf.len() as i32,
                &mut out_len as *mut u32 as i32,
            )
        };
        if rc == 0 {
            // Sukces: out_len niesie długość metadanych CBOR. Długość bajtów
            // kawałka czytamy z `chunk_len` w metadanych.
            meta_buf.truncate(out_len as usize);
            let meta: DocumentGetMeta = decode_cbor(&meta_buf)?;
            blob_buf.truncate(meta.chunk_len as usize);
            return Ok((blob_buf, meta));
        }
        if rc == AbiError::OutputBufferTooSmall.as_i32() {
            let required = out_len as usize;
            if attempts > MAX_RETRY_ATTEMPTS || required <= blob_cap {
                return Err(AbiError::OutputBufferTooSmall);
            }
            if required > MAX_OUT_CAP {
                return Err(AbiError::PayloadTooLarge);
            }
            blob_cap = required;
            continue;
        }
        return Err(AbiError::from_i32(rc));
    }
}

// =============================================================================
// Graph API wrappers (RAG 0.2) — embedded CozoDB per-addon per-collection graphs
// =============================================================================

pub use tentaflow_sdk_spec::protocol::graph::GraphNode;
pub use tentaflow_sdk_spec::{
    GraphDirection, GraphNeighbor, GraphProp, GraphRankedNode, GraphSeed, Provenance,
};

/// Insert or replace a node in `collection`. Returns the post-upsert node count.
/// Requires `graph.write`; the collection must be declared under
/// `[[graph_collection]]` in the addon manifest.
pub fn graph_upsert_node(collection: &str, node: GraphNode) -> Result<u64, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::GraphUpsertNodeInput {
        collection: collection.to_string(),
        node,
    })?;
    let bytes = call_sql_with_one_input(graph_upsert_node_v1, &payload)?;
    let resp: tentaflow_sdk_spec::GraphUpsertNodeOutput = decode_cbor(&bytes)?;
    Ok(resp.count)
}

/// Insert or replace a directed edge `src -[rel]-> dst`. `weight = None` → 1.0.
/// Returns the post-upsert edge count. Requires `graph.write`.
pub fn graph_upsert_edge(
    collection: &str,
    src: &str,
    rel: &str,
    dst: &str,
    weight: Option<f64>,
    props: Vec<GraphProp>,
    provenance: Option<Provenance>,
) -> Result<u64, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::GraphUpsertEdgeInput {
        collection: collection.to_string(),
        src: src.to_string(),
        rel: rel.to_string(),
        dst: dst.to_string(),
        weight,
        props,
        provenance,
    })?;
    let bytes = call_sql_with_one_input(graph_upsert_edge_v1, &payload)?;
    let resp: tentaflow_sdk_spec::GraphUpsertEdgeOutput = decode_cbor(&bytes)?;
    Ok(resp.count)
}

/// Adjacency of `node` in `direction`, optionally filtered by `rel`, capped at
/// `limit`. Requires `graph.read`.
pub fn graph_neighbors(
    collection: &str,
    node: &str,
    direction: GraphDirection,
    rel: Option<&str>,
    limit: u32,
) -> Result<Vec<GraphNeighbor>, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::GraphNeighborsInput {
        collection: collection.to_string(),
        node: node.to_string(),
        direction,
        rel: rel.map(str::to_string),
        limit,
    })?;
    let bytes = call_sql_with_one_input(graph_neighbors_v1, &payload)?;
    let resp: tentaflow_sdk_spec::GraphNeighborsOutput = decode_cbor(&bytes)?;
    Ok(resp.neighbors)
}

/// Built-in Cozo PageRank, top-N nodes (highest first). Requires `graph.read`.
pub fn graph_pagerank(
    collection: &str,
    top_n: u32,
    damping: Option<f64>,
    iterations: Option<u32>,
) -> Result<Vec<GraphRankedNode>, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::GraphPagerankInput {
        collection: collection.to_string(),
        top_n,
        damping,
        iterations,
    })?;
    let bytes = call_sql_with_one_input(graph_pagerank_v1, &payload)?;
    let resp: tentaflow_sdk_spec::GraphPagerankOutput = decode_cbor(&bytes)?;
    Ok(resp.ranked)
}

/// Personalized PageRank with `seeds` as the personalization vector, top-N nodes.
/// Requires `graph.read`.
pub fn graph_ppr(
    collection: &str,
    seeds: Vec<GraphSeed>,
    top_n: u32,
    damping: Option<f64>,
    iterations: Option<u32>,
) -> Result<Vec<GraphRankedNode>, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::GraphPprInput {
        collection: collection.to_string(),
        seeds,
        top_n,
        damping,
        iterations,
    })?;
    let bytes = call_sql_with_one_input(graph_ppr_v1, &payload)?;
    let resp: tentaflow_sdk_spec::GraphPprOutput = decode_cbor(&bytes)?;
    Ok(resp.ranked)
}

/// Delete a node (and its edges). Returns `true` if it existed. `graph.write`.
pub fn graph_delete_node(collection: &str, id: &str) -> Result<bool, AbiError> {
    graph_delete(collection, tentaflow_sdk_spec::GraphDeleteTarget::Node(id.to_string()))
}

/// Delete a single edge `(src, rel, dst)`. Returns `true` if it existed.
pub fn graph_delete_edge(
    collection: &str,
    src: &str,
    rel: &str,
    dst: &str,
) -> Result<bool, AbiError> {
    graph_delete(
        collection,
        tentaflow_sdk_spec::GraphDeleteTarget::Edge(
            src.to_string(),
            rel.to_string(),
            dst.to_string(),
        ),
    )
}

/// Soft-delete (tombstone) a node — keeps it for provenance chains but hides it
/// from retrieval. Returns `true` if it existed. `graph.write`.
pub fn graph_tombstone_node(collection: &str, id: &str) -> Result<bool, AbiError> {
    graph_delete(collection, tentaflow_sdk_spec::GraphDeleteTarget::Tombstone(id.to_string()))
}

fn graph_delete(
    collection: &str,
    target: tentaflow_sdk_spec::GraphDeleteTarget,
) -> Result<bool, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::GraphDeleteInput {
        collection: collection.to_string(),
        target,
    })?;
    let bytes = call_sql_with_one_input(graph_delete_v1, &payload)?;
    let resp: tentaflow_sdk_spec::GraphDeleteOutput = decode_cbor(&bytes)?;
    Ok(resp.removed)
}

// =============================================================================
// Policy / Gate API wrappers (F1c P4) — DPIA / FRIA claim verification
// =============================================================================

/// One signer entry on a verified claim. `role` matches the manifest gate
/// requirement (`dpo`, `supervisor`, ...) and `user` is the admin identity
/// recorded when the claim was issued.
#[derive(Debug, Clone, Deserialize)]
pub struct GateSigner {
    pub role: String,
    pub user: String,
}

/// Result of `gate_check`. `valid=true` means the claim satisfied every
/// requirement of the named gate (validity window, type, scope, signer
/// roles). When `valid=false`, `reason` carries a human-readable denial
/// message from the policy engine.
#[derive(Debug, Clone, Deserialize)]
pub struct GateCheckResult {
    pub valid: bool,
    pub claim_id: String,
    #[serde(default)]
    pub claim_type: String,
    #[serde(default)]
    pub valid_until: String,
    #[serde(default)]
    pub signers: Vec<GateSigner>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Verify a policy claim against the gate id declared in the addon manifest.
/// Requires `policy.read` permission. A `valid=false` result is NOT an
/// AbiError — it is a soft decision the addon can react to. Hard errors
/// (no permission, gate id unknown, malformed payload) return AbiError.
pub fn gate_check(gate_id: &str, claim_id: &str) -> Result<GateCheckResult, AbiError> {
    gate_check_scoped(gate_id, claim_id, None)
}

/// Like `gate_check` but with an optional resource scope hint — required
/// when the claim was issued for a specific resource (vector namespace,
/// alias id) and the gate enforces namespace-level narrowing.
pub fn gate_check_scoped(
    gate_id: &str,
    claim_id: &str,
    resource_scope: Option<&str>,
) -> Result<GateCheckResult, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::GateCheckInput {
        gate_id: gate_id.to_string(),
        claim_id: claim_id.to_string(),
        resource_scope: resource_scope.map(str::to_string),
    })?;
    let bytes = call_sql_with_one_input(gate_check_v1, &payload)?;
    let out: tentaflow_sdk_spec::GateCheckOutput = decode_cbor(&bytes)?;
    Ok(GateCheckResult {
        valid: out.valid,
        claim_id: out.claim_id,
        claim_type: out.claim_type,
        valid_until: out.valid_until,
        signers: out
            .signers
            .into_iter()
            .map(|s| GateSigner {
                role: s.role,
                user: s.user,
            })
            .collect(),
        reason: out.reason,
    })
}

// =============================================================================
// Wysokopoziomowe wrappery — Flow API (F1c P5)
// =============================================================================

/// Status row returned by `flow_invoke` / `flow_status`. Mirrors the
/// `InvocationStatus` shape produced by the core scheduler.
#[derive(Debug, Clone, Deserialize)]
pub struct FlowInvocation {
    pub invocation_id: String,
    pub status: String,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub operators_completed: i64,
    #[serde(default)]
    pub operators_total: i64,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub result_toml: Option<String>,
}

fn flow_invocation_from(out: tentaflow_sdk_spec::FlowInvocationOutput) -> FlowInvocation {
    FlowInvocation {
        invocation_id: out.invocation_id,
        status: out.status,
        started_at: out.started_at,
        finished_at: out.finished_at,
        operators_completed: out.operators_completed,
        operators_total: out.operators_total,
        error: out.error,
        result_toml: out.result_toml,
    }
}

/// Invoke a manifest-declared flow. `wait_ms == 0` returns immediately with
/// `status = "running"`; `wait_ms > 0` awaits the DAG up to 30 s (silently
/// clamped by the host). `input` is forwarded verbatim to every operator
/// as `OperatorContext.input_toml`.
pub fn flow_invoke(
    flow_id: &str,
    input_toml: &str,
    wait_ms: u32,
) -> Result<FlowInvocation, AbiError> {
    // `input_toml` is the opaque operator payload — carried verbatim as a string
    // inside the CBOR input and parsed back into a `toml::Value` host-side.
    let trimmed = input_toml.trim();
    let payload = encode_cbor_input(&tentaflow_sdk_spec::FlowInvokeInput {
        flow_id: flow_id.to_string(),
        input_toml: if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        },
        wait_ms,
    })?;
    let bytes = call_sql_with_one_input(flow_invoke_v1, &payload)?;
    let out: tentaflow_sdk_spec::FlowInvocationOutput = decode_cbor(&bytes)?;
    Ok(flow_invocation_from(out))
}

/// Read the authoritative DB row for an invocation. The host filters by
/// the calling addon id, so an invocation owned by a different addon is
/// reported as `AbiError::NotFound`.
pub fn flow_status(invocation_id: &str) -> Result<FlowInvocation, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::FlowInvocationIdInput {
        invocation_id: invocation_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(flow_status_v1, &payload)?;
    let out: tentaflow_sdk_spec::FlowInvocationOutput = decode_cbor(&bytes)?;
    Ok(flow_invocation_from(out))
}

/// Request cooperative cancellation of a running invocation. Idempotent:
/// cancelling a finished invocation returns `cancelled = true` as long as
/// the invocation belongs to the calling addon.
pub fn flow_cancel(invocation_id: &str) -> Result<bool, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::FlowInvocationIdInput {
        invocation_id: invocation_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(flow_cancel_v1, &payload)?;
    let out: tentaflow_sdk_spec::FlowCancelOutput = decode_cbor(&bytes)?;
    Ok(out.cancelled)
}

// =============================================================================
// Services catalog wrappers (F2 P2.a) — read-only mesh service inspection
// =============================================================================

/// One row from `service_list`. `service_id` is the cross-node stable id
/// `<node>:<local_id>`; `service_local_id` carries the numeric id that the
/// router APIs expect when addressing the service directly.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceInfo {
    pub service_id: String,
    pub service_local_id: i64,
    pub display_name: String,
    pub kind: String,
    pub status: String,
    pub node_id: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Filtered view of every service visible in the mesh (local node + every
/// reachable peer). Pass `None` for any filter to include everything.
/// Requires the `service.read` permission.
pub fn service_list(
    kind: Option<&str>,
    status: Option<&str>,
    node_id: Option<&str>,
) -> Result<Vec<ServiceInfo>, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::ServiceListInput {
        kind: kind.map(str::to_string),
        status: status.map(str::to_string),
        node_id: node_id.map(str::to_string),
    })?;
    let bytes = call_sql_with_one_input(service_list_v1, &payload)?;
    let resp: tentaflow_sdk_spec::ServiceListOutput = decode_cbor(&bytes)?;
    Ok(resp
        .services
        .into_iter()
        .map(|s| ServiceInfo {
            service_id: s.service_id,
            service_local_id: s.service_local_id,
            display_name: s.display_name,
            kind: s.kind,
            status: s.status,
            node_id: s.node_id,
            endpoint: s.endpoint,
            capabilities: s.capabilities,
        })
        .collect())
}

/// Live hardware snapshot for one node. Local node only today — passing an
/// unknown / remote `node_id` returns `AbiError::NotFound`. Requires the
/// `service.read` permission.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeResources {
    pub node_id: String,
    pub cpu_cores: u32,
    pub cpu_load_pct: f64,
    pub ram_total_mb: u64,
    pub ram_used_mb: u64,
    #[serde(default)]
    pub gpu: Option<NodeGpu>,
    pub gpu_count: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeGpu {
    pub name: String,
    pub vram_total_mb: u64,
    pub vram_used_mb: u64,
    pub utilization_pct: f64,
}

pub fn node_resources_get(node_id: &str) -> Result<NodeResources, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::NodeResourcesInput {
        node_id: node_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(node_resources_get_v1, &payload)?;
    let out: tentaflow_sdk_spec::NodeResourcesOut = decode_cbor(&bytes)?;
    Ok(NodeResources {
        node_id: out.node_id,
        cpu_cores: out.cpu_cores,
        cpu_load_pct: out.cpu_load_pct,
        ram_total_mb: out.ram_total_mb,
        ram_used_mb: out.ram_used_mb,
        gpu: out.gpu.map(|g| NodeGpu {
            name: g.name,
            vram_total_mb: g.vram_total_mb,
            vram_used_mb: g.vram_used_mb,
            utilization_pct: g.utilization_pct,
        }),
        gpu_count: out.gpu_count,
    })
}

// =============================================================================
// Alokator pamieci WASM — eksportowany dla hosta
// =============================================================================

/// Alokuje bufor w pamieci guest WASM.
/// Eksportowane jako funkcja WASM "alloc" dla hosta.
#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let layout = std::alloc::Layout::from_size_align(size as usize, 1)
        .expect("Niepoprawny layout alokacji");
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        return 0;
    }
    ptr as i32
}

/// Zwalnia bufor w pamieci guest WASM.
/// Eksportowane jako funkcja WASM "dealloc" dla hosta.
#[no_mangle]
pub extern "C" fn dealloc(ptr: i32, size: i32) {
    if ptr == 0 || size <= 0 {
        return;
    }
    let layout = std::alloc::Layout::from_size_align(size as usize, 1)
        .expect("Niepoprawny layout dealokacji");
    unsafe {
        std::alloc::dealloc(ptr as *mut u8, layout);
    }
}

#[cfg(test)]
mod state_wrapper_tests {
    use super::*;

    // The state wrappers must never erase a host error into None/false/empty.
    // The error path is the pure `From<AbiError> for StateError` mapping the
    // wrappers apply, so we assert that mapping precisely here.
    #[test]
    fn abi_error_maps_to_state_error() {
        assert_eq!(StateError::from(AbiError::Permission), StateError::Permission);
        assert_eq!(
            StateError::from(AbiError::PayloadTooLarge),
            StateError::ValueTooLarge
        );
        assert_eq!(
            StateError::from(AbiError::QuotaExceeded),
            StateError::QuotaExceeded
        );
        // Any other code is carried through as Other(..) — never swallowed.
        assert_eq!(
            StateError::from(AbiError::Operation),
            StateError::Other(AbiError::Operation)
        );
    }

    // state_get returns Ok(None) for NotFound but Err for a real failure: the
    // NotFound code is the only one mapped to a successful "absent" result.
    #[test]
    fn not_found_is_absent_not_error() {
        // NotFound must not become an Err (it is a normal absent outcome).
        assert_eq!(AbiError::from_i32(2), AbiError::NotFound);
        // Permission must map to an Err variant, never to absent.
        assert_eq!(StateError::from(AbiError::NotFound), StateError::Other(AbiError::NotFound));
        assert_ne!(StateError::from(AbiError::Permission), StateError::Other(AbiError::NotFound));
    }

    #[test]
    fn state_list_output_cap_matches_host_ceiling() {
        // The list output cap must be >= the host's 8 MiB ServiceCall ceiling so
        // a host-legal response is never silently dropped as over-cap.
        assert!(MAX_OUT_CAP_STATE_LIST >= 8 * 1024 * 1024);
        assert!(MAX_OUT_CAP_STATE_LIST > MAX_OUT_CAP);
    }

    #[test]
    fn tier_from_wire_roundtrip() {
        assert_eq!(StateTier::from_wire(StateTier::Durable.to_wire()), StateTier::Durable);
        assert_eq!(
            StateTier::from_wire(StateTier::Ephemeral.to_wire()),
            StateTier::Ephemeral
        );
    }
}
