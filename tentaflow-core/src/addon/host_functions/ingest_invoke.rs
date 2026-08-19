// =============================================================================
// Plik: addon/host_functions/ingest_invoke.rs
// Opis: Host function `ingest_invoke_v1` (RAG Partia 3 prerequisite) — uruchamia
//       flow-ingest JEDNEGO dokumentu z BINARNYM payloadem. Addon podaje
//       `doc_id_blob` (referencję do per-instance document store) + mime + nazwę
//       flow + opcje; core pobiera bajty PO SWOJEJ STRONIE (zero podwójnego
//       transferu pliku przez ABI), seeduje binarny envelope i dispatchuje flow
//       `<model>:ingest:document`. To JEDYNA ścieżka wywołania flow-ingestu z
//       surowym dokumentem (PDF/xlsx/docx/obraz) — `llm_generate` buduje tylko
//       tekstową wiadomość. Lustro `doc_parse_v1`: czyta CBOR input, woła
//       `ModelRuntimeExecutor::execute_ingest`, zwraca CBOR output.
// Uprawnienia: "document.read" (czyta document store instancji). Audit RiskClass::B.
// =============================================================================

use tentaflow_sdk_spec::{IngestInvokeInput, IngestInvokeOutput};

use super::abi_helpers::PayloadKind;
use super::cbor_io::{read_input_cbor, write_cbor_capped};
use super::document::read_full_document;
use super::{audit_log_with_risk, check_permission, get_memory, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::services::runtime::context::ExecutionContext;
use crate::services::runtime::executor::IngestRequest;

const PERM_DOCUMENT_READ: &str = "document.read";

fn audit(state: &AddonState, result: &str, reason: Option<&str>) {
    audit_log_with_risk(
        state,
        "ingest.invoke",
        Some("document"),
        None,
        RiskClass::B,
        None,
        None,
        result,
        reason,
    );
}

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
///
/// Input CBOR: `IngestInvokeInput { doc_id_blob, mime, model, options_json? }`.
/// Output CBOR: `IngestInvokeOutput { markdown, chunks, page_count }` (teksty
/// chunków NIE wracają przez ABI — addon czyta je z przestrzeni `passages` po
/// `doc_id`, by duży dokument nie przekroczył capu 8 MiB).
/// Wymaga `document.read`. Risk class B — dokumenty mogą nieść dane regulowane.
pub fn ingest_invoke_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => {
            audit(caller.data(), "error", Some("memory_unavailable"));
            return AbiError::Operation.as_i32();
        }
    };

    if !check_permission(caller.data(), PERM_DOCUMENT_READ, None) {
        audit(caller.data(), "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }

    let input: IngestInvokeInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "denied",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };

    if input.doc_id_blob.is_empty() || input.mime.is_empty() || input.model.is_empty() {
        audit(caller.data(), "denied", Some("missing_required_field"));
        return AbiError::Operation.as_i32();
    }

    // Opcje to opaque JSON z addona (collection_id, graph toggle, params).
    // Musi być obiektem JSON — wsiąka do flow.meta klucz po kluczu. Pusty/None
    // = brak opcji. Niepoprawny JSON (lub nie-obiekt) odrzucamy, zamiast cicho
    // gubić konfigurację ingestu.
    let options = match input.options_json.as_deref() {
        None | Some("") => serde_json::Map::new(),
        Some(raw) => match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(serde_json::Value::Object(map)) => map,
            Ok(_) => {
                audit(caller.data(), "denied", Some("options_not_object"));
                return AbiError::Operation.as_i32();
            }
            Err(_) => {
                audit(caller.data(), "denied", Some("options_invalid_json"));
                return AbiError::Operation.as_i32();
            }
        },
    };

    let addon_id = caller.data().addon_id.clone();
    let org_id = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    let user_id = caller.data().user_id.clone();

    // Bajty dokumentu pobrane PO STRONIE HOSTA z document store instancji
    // callera — `read_full_document` widzi tylko dokumenty tej instancji
    // (izolacja per addon_id), więc obcy `doc_id` zwraca NotFound.
    let (document_bytes, _stored_mime) =
        match read_full_document(&org_id, &addon_id, &input.doc_id_blob, user_id.as_deref()) {
            Ok(pair) => pair,
            Err(AbiError::NotFound) => {
                audit(caller.data(), "denied", Some("document_not_found"));
                return AbiError::NotFound.as_i32();
            }
            Err(e) => {
                audit(caller.data(), "error", Some("document_read_failed"));
                return e.as_i32();
            }
        };
    if document_bytes.is_empty() {
        audit(caller.data(), "denied", Some("document_empty"));
        return AbiError::Operation.as_i32();
    }

    // Executor żyje pod routerem. Brak routera (DB-less harness) → operacja
    // niedostępna, nie cichy sukces.
    let executor = match caller.data().router.as_ref().and_then(|r| r.executor()) {
        Some(e) => e,
        None => {
            audit(caller.data(), "error", Some("executor_unavailable"));
            return AbiError::Operation.as_i32();
        }
    };

    let request = IngestRequest {
        model: input.model,
        document_bytes,
        mime: input.mime,
        options,
        // Addon zawsze pisze do wlasnego drzewa — hosta nie da sie stad
        // przekierowac na cudzy katalog.
        vector_home: None,
        cancel_token: None,
        flow_depth: 0,
    };

    // Tożsamość callera trafia do `ctx.user` (flow-meta bierze user z `ctx.user`)
    // oraz `ctx.addon_id`/`org_id` (przestrzeń instancji). `AddonState` nie nosi
    // roli — addon-caller dostaje domyślną rolę `user` (brak admin-bypassu).
    let caller_user = user_id
        .filter(|id| !id.is_empty())
        .map(|id| crate::auth::acl::UserContext::new(id, "user"));
    let mut ctx = ExecutionContext::new(
        caller_user,
        crate::flow_engine::dispatcher::FlowOrigin::Addon,
        crate::flow_engine::dispatcher::FlowActor::addon(addon_id.clone()),
    )
    .with_addon_identity(Some(addon_id), Some(org_id));

    // Most async→sync: host function jest synchroniczna, executor async.
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(executor.execute_ingest(request, &mut ctx))
    });

    let response = match result {
        Ok(r) => r,
        Err(e) => {
            audit(caller.data(), "error", Some(&e.to_string()));
            return AbiError::Operation.as_i32();
        }
    };

    let out = IngestInvokeOutput {
        markdown: response.markdown,
        chunks: response.chunks,
        page_count: response.page_count,
    };

    // Audyt "ok" DOPIERO po udanym zapisie wyniku do pamięci WASM.
    let write_code = write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    );

    if write_code == AbiError::Ok.as_i32() {
        audit(caller.data(), "ok", None);
    } else {
        audit(caller.data(), "error", Some("write_output_failed"));
    }

    write_code
}
