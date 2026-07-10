// =============================================================================
// Plik: addon/host_functions/doc_parse.rs
// Opis: Host function `doc_parse_v1` (RAG E1.2) — parsuje OBRAZ strony dokumentu
//       na markdown + bloki layoutu przez serwis vision-parse (alias
//       `rag-parse` → np. nemotron-parse). Orkiestracja w core: czyta CBOR
//       input z pamieci WASM, woła `ModelRuntimeExecutor::execute_documents`
//       (resolve ServiceSurface::Documents → rank → dispatch per backend z
//       failoverem), zwraca CBOR output. Backend pluggable: serwer = zdalny
//       HTTP; telefon = embedded (gniazdo, błąd→fallback).
// Uprawnienia: "document.parse" (fail-closed). Audit RiskClass::B.
// =============================================================================

use base64::Engine;
use tentaflow_sdk_spec::{DocBlock as WireDocBlock, DocParseInput, DocParseOutput};

use super::abi_helpers::PayloadKind;
use super::cbor_io::{read_input_cbor, write_cbor_capped};
use super::{audit_log_with_risk, check_permission, get_memory, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::services::runtime::context::ExecutionContext;
use crate::services::runtime::executor::DocumentParseRequest;

const PERM_DOCUMENT_PARSE: &str = "document.parse";

/// Domyślny alias gdy addon nie poda `model_alias`. Alias-aware failover jak
/// `rag-reranker` — resolver schodzi na kolejne instancje przy awarii primary.
const DEFAULT_PARSE_ALIAS: &str = "rag-parse";

fn audit(state: &AddonState, result: &str, reason: Option<&str>) {
    audit_log_with_risk(
        state,
        "document.parse",
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
/// Input CBOR: `DocParseInput { image_b64, mime, model_alias? }`.
/// Output CBOR: `DocParseOutput { markdown, blocks, page_count }`.
/// Wymaga `document.parse`. Risk class B — dokumenty mogą nieść dane regulowane.
pub fn doc_parse_v1(
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

    if !check_permission(caller.data(), PERM_DOCUMENT_PARSE, None) {
        audit(caller.data(), "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }

    let input: DocParseInput = match read_input_cbor(
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

    let image_bytes =
        match base64::engine::general_purpose::STANDARD.decode(input.image_b64.as_bytes()) {
            Ok(b) if !b.is_empty() => b,
            Ok(_) => {
                audit(caller.data(), "denied", Some("image_empty"));
                return AbiError::Operation.as_i32();
            }
            Err(_) => {
                audit(caller.data(), "denied", Some("image_b64_invalid"));
                return AbiError::Operation.as_i32();
            }
        };

    let model = input
        .model_alias
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_PARSE_ALIAS.to_string());

    // Executor żyje pod routerem. Brak routera (DB-less harness) → operacja
    // niedostępna, nie cichy sukces.
    let executor = match caller.data().router.as_ref().and_then(|r| r.executor()) {
        Some(e) => e,
        None => {
            audit(caller.data(), "error", Some("executor_unavailable"));
            return AbiError::Operation.as_i32();
        }
    };

    // Tożsamość addona-callera przeprowadzana do `ExecutionContext` (jak w
    // service_call.rs), żeby flow-target parsera trafiał w przestrzeń instancji.
    let addon_id = caller.data().addon_id.clone();
    let org_id = caller.data().org_id.clone();
    let user_id = caller.data().user_id.clone();

    let request = DocumentParseRequest {
        model,
        image_bytes,
        mime: input.mime,
        flow_depth: 0,
    };

    // Tożsamość callera musi trafić do `ctx.user`, bo flow-meta bierze user z
    // `ctx.user` (nie z pól request). `AddonState` nie nosi roli — addon-caller
    // dostaje domyślną rolę `user` (brak admin-bypassu). Bez user_id ctx zostaje
    // bez usera (wywołanie systemowe).
    let caller_user = user_id
        .filter(|id| !id.is_empty())
        .map(|id| crate::auth::acl::UserContext::new(id, "user"));

    let mut ctx = ExecutionContext::new(caller_user).with_addon_identity(Some(addon_id), org_id);

    // Most async→sync: host function jest synchroniczna, executor async.
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(executor.execute_documents(request, &mut ctx))
    });

    let response = match result {
        Ok(r) => r,
        Err(e) => {
            audit(caller.data(), "error", Some(&e.to_string()));
            return AbiError::Operation.as_i32();
        }
    };

    let out = DocParseOutput {
        page_count: response
            .blocks
            .iter()
            .map(|b| b.page.saturating_add(1))
            .max()
            .unwrap_or(1),
        markdown: response.markdown,
        blocks: response
            .blocks
            .into_iter()
            .map(|b| WireDocBlock {
                page: b.page,
                class: b.class,
                bbox: b.bbox,
                text: b.text,
                confidence: b.confidence,
            })
            .collect(),
    };

    // Audyt "ok" DOPIERO po udanym zapisie wyniku do pamięci WASM. Gdy write
    // padnie (np. bufor za mały, enkodowanie), audyt nie może kłamać "ok".
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
