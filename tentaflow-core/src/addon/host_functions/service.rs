// =============================================================================
// Plik: addon/host_functions/service.rs
// Opis: Host function Service API — wysylanie requestow do zarejestrowanych
//       serwisow (kontenerow Docker) przez infrastrukture QUIC routera.
//       Addon podaje nazwe serwisu i JSON payload, Core routuje przez QUIC.
// Uprawnienia: "service" (globalny) oraz "service" z resource=<service_name>
//              (per-service whitelist). Obydwa musza byc przyznane — fail-closed.
// =============================================================================

use tracing::{error, info, warn};

use super::{
    audit_log, check_permission, get_memory, read_guest_string, write_guest_output, AddonState,
    WasmCaller, ABI_ERR_NOT_FOUND, ABI_ERR_OPERATION, ABI_ERR_PERMISSION, ABI_ERR_RATE_LIMIT,
};

use crate::addon::rate_limiter::ResourceType;

// =============================================================================
// service_request — wyslanie requestu do nazwanego serwisu przez QUIC
// =============================================================================

/// Host function: wysyla request do zarejestrowanego serwisu (kontenera Docker).
///
/// ABI:
/// - service_name_ptr/service_name_len: nazwa serwisu (UTF-8)
/// - request_json_ptr/request_json_len: JSON payload do wyslania
/// - out_ptr/out_cap: bufor na odpowiedz JSON
/// - out_len_ptr: ile bajtow zapisano
/// - Zwraca: ABI_OK lub kod bledu
pub fn service_request(
    mut caller: WasmCaller<'_, AddonState>,
    service_name_ptr: i32,
    service_name_len: i32,
    request_json_ptr: i32,
    request_json_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    // Odczytaj nazwe serwisu z pamieci WASM
    let service_name = match read_guest_string(&memory, &caller, service_name_ptr, service_name_len)
    {
        Some(s) => s.to_string(),
        None => {
            warn!("service_request: niepoprawny wskaznik service_name");
            return ABI_ERR_OPERATION;
        }
    };

    // Odczytaj request JSON z pamieci WASM
    let request_json = match read_guest_string(&memory, &caller, request_json_ptr, request_json_len)
    {
        Some(s) => s.to_string(),
        None => {
            warn!("service_request: niepoprawny wskaznik request_json");
            return ABI_ERR_OPERATION;
        }
    };

    // Sprawdz uprawnienie "service"
    if !check_permission(caller.data(), "service", None) {
        audit_log(
            caller.data(),
            "service.request",
            Some("service"),
            Some(&service_name),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    // Sprawdz uprawnienie do konkretnego serwisu
    if !check_permission(caller.data(), "service", Some(&service_name)) {
        audit_log(
            caller.data(),
            "service.request",
            Some("service"),
            Some(&service_name),
            "denied",
            Some(&format!("serwis '{}' niedozwolony", service_name)),
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();
    info!(
        "service_request: addon='{}', service='{}', payload_len={}",
        addon_id,
        service_name,
        request_json.len()
    );

    // Sprawdz rate limit
    if let Some(ref rate_limiter) = caller.data().rate_limiter {
        if rate_limiter
            .check(&addon_id, ResourceType::HttpRequests)
            .is_err()
        {
            audit_log(
                caller.data(),
                "service.request",
                Some("service"),
                Some(&service_name),
                "error",
                Some("rate limit exceeded"),
            );
            return ABI_ERR_RATE_LIMIT;
        }
        rate_limiter.record_usage(&addon_id, ResourceType::HttpRequests, 1);
    }

    // Pobierz router z AddonState
    let router = match caller.data().router.as_ref() {
        Some(r) => r.clone(),
        None => {
            warn!(
                "service_request: router niedostepny dla addon='{}'",
                addon_id
            );
            audit_log(
                caller.data(),
                "service.request",
                Some("service"),
                Some(&service_name),
                "error",
                Some("router unavailable"),
            );
            return ABI_ERR_OPERATION;
        }
    };

    // Znajdz QUIC client dla serwisu — szukamy po kolei w roznych typach
    let service_manager = router.service_manager();

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            dispatch_to_service(service_manager, &service_name, &request_json, &addon_id).await
        })
    });

    match result {
        Ok(response_json) => {
            let response_bytes = response_json.as_bytes();

            audit_log(
                caller.data(),
                "service.request",
                Some("service"),
                Some(&service_name),
                "ok",
                None,
            );

            write_guest_output(
                &memory,
                &mut caller,
                out_ptr,
                out_cap,
                out_len_ptr,
                response_bytes,
            )
        }
        Err(err_code) => {
            let err_msg = match err_code {
                ABI_ERR_NOT_FOUND => format!("serwis '{}' nie znaleziony", service_name),
                _ => format!("blad wysylania do serwisu '{}'", service_name),
            };
            error!("service_request: addon='{}': {}", addon_id, err_msg);
            audit_log(
                caller.data(),
                "service.request",
                Some("service"),
                Some(&service_name),
                "error",
                Some(&err_msg),
            );
            err_code
        }
    }
}

/// Wysyla request do serwisu przez QUIC — probuje znalezc klienta
/// w mapach LLM, Embedding, TTS, STT i Memory.
async fn dispatch_to_service(
    service_manager: &crate::routing::service_manager::ServiceManager,
    service_name: &str,
    request_json: &str,
    addon_id: &str,
) -> std::result::Result<String, i32> {
    use tentaflow_protocol::*;

    // Szukaj QUIC client w kolejnosci: LLM, Embedding, TTS, STT
    let quic_client = service_manager
        .get_quic_llm_client(service_name)
        .await
        .or(service_manager
            .get_quic_embedding_client(service_name)
            .await)
        .or(service_manager.get_quic_tts_client(service_name).await)
        .or(service_manager.get_quic_stt_client(service_name).await);

    let quic_client = match quic_client {
        Some(c) => c,
        None => {
            warn!(
                "service_request: brak QUIC klienta dla serwisu '{}'",
                service_name
            );
            return Err(ABI_ERR_NOT_FOUND);
        }
    };

    // Zbuduj ModelRequest z JSON payload w polu prompt CompletionPayload
    let request_id = uuid::Uuid::new_v4().to_string();
    let model_request = ModelRequest {
        request_id,
        payload: ModelPayload::Completion(CompletionPayload {
            model: service_name.to_string(),
            prompt: Some(request_json.to_string()),
            messages: vec![Message {
                role: "user".to_string(),
                content: request_json.to_string(),
            }],
            temperature: None,
            max_tokens: None,
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tts_options: None,
            memory_options: None,
            audio_input: None,
            prefix_cache_id: None,
            prefix_text: None,
        }),
        stream: false,
        metadata: Some(vec![("addon_id".to_string(), addon_id.to_string())]),
        session_id: None,
    };

    let model_response = quic_client.send_request(model_request).await.map_err(|e| {
        error!("service_request: blad QUIC dla '{}': {}", service_name, e);
        ABI_ERR_OPERATION
    })?;

    // Serializuj cala odpowiedz jako JSON
    let response = match model_response.result {
        ModelResult::Completion(ref completion) => {
            serde_json::json!({
                "status": "ok",
                "request_id": model_response.request_id,
                "text": completion.text,
                "model": completion.model,
                "finish_reason": completion.finish_reason,
            })
        }
        ModelResult::Error(ref err) => {
            serde_json::json!({
                "status": "error",
                "request_id": model_response.request_id,
                "error": err.message,
            })
        }
        _ => {
            // Inne typy odpowiedzi — zwroc surowy opis
            serde_json::json!({
                "status": "ok",
                "request_id": model_response.request_id,
                "result_type": format!("{:?}", std::mem::discriminant(&model_response.result)),
            })
        }
    };

    serde_json::to_string(&response).map_err(|_| ABI_ERR_OPERATION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::event_bus::EventBus;
    use crate::addon::host_functions::check_permission;
    use crate::addon::host_functions::network::NetworkConnectionManager;
    use crate::addon::permissions::PermissionChecker;
    use crate::addon::AddonManifest;
    use parking_lot::Mutex;
    use std::path::Path;
    use std::sync::Arc;

    /// Tworzy in-memory DB z pelnym schematem
    fn create_test_db() -> crate::db::DbPool {
        crate::db::init(Path::new(":memory:")).expect("Nie udalo sie utworzyc test DB")
    }

    /// Tworzy minimalny AddonState do testowania check_permission
    fn create_test_addon_state(
        addon_id: &str,
        permissions: Vec<String>,
        user_id: Option<i64>,
        is_system_call: bool,
    ) -> AddonState {
        let db = create_test_db();
        let event_bus = Arc::new(EventBus::new());
        let permission_checker = Arc::new(PermissionChecker::new(db.clone()));
        let settings_cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));

        AddonState {
            addon_id: addon_id.to_string(),
            instance_id: "test-instance".to_string(),
            user_id,
            db,
            permissions,
            event_bus,
            permission_checker,
            fuel_consumed: 0,
            is_system_call,
            rate_limiter: None,
            net_manager: Arc::new(Mutex::new(NetworkConnectionManager::new())),
            settings_cipher,
            manifest: Arc::new(AddonManifest::default()),
            memory_limit: 64 * 1024 * 1024,
            oauth_refresh_guard: std::sync::Arc::new(
                crate::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
            ),
            router: None,
        }
    }

    #[test]
    fn check_permission_no_service_permission_returns_false() {
        // Addon bez uprawnienia "service" — check_permission zwraca false

        // Arrange
        let state = create_test_addon_state(
            "test-addon",
            vec!["llm".to_string(), "storage".to_string()],
            None,
            true,
        );

        // Act & Assert
        assert!(
            !check_permission(&state, "service", None),
            "Addon bez 'service' w permissions powinien byc odrzucony"
        );
    }

    #[test]
    fn check_permission_with_service_permission_system_call_returns_true() {
        // Addon z uprawnieniem "service" i is_system_call=true — check_permission zwraca true

        // Arrange
        let state = create_test_addon_state(
            "teams-bot",
            vec!["service".to_string(), "llm".to_string()],
            None,
            true,
        );

        // Act & Assert
        assert!(
            check_permission(&state, "service", None),
            "Addon z 'service' i is_system_call=true powinien miec uprawnienie"
        );
    }

    #[test]
    fn check_permission_without_system_call_no_user_returns_false() {
        // Addon z uprawnieniem "service" ale bez user_id i is_system_call=false

        // Arrange
        let state =
            create_test_addon_state("untrusted-addon", vec!["service".to_string()], None, false);

        // Act & Assert
        assert!(
            !check_permission(&state, "service", None),
            "Addon bez user_id i is_system_call=false powinien byc odrzucony"
        );
    }

    #[test]
    fn check_permission_service_with_resource_name_no_permission_returns_false() {
        // Addon bez "service" w permissions — nawet z nazwa serwisu nie przejdzie

        // Arrange
        let state = create_test_addon_state("test-addon", vec!["llm".to_string()], None, true);

        // Act & Assert
        assert!(
            !check_permission(&state, "service", Some("teams-stt")),
            "Addon bez 'service' nie powinien miec dostepu do zadnego serwisu"
        );
    }

    #[test]
    fn check_permission_empty_permissions_returns_false() {
        // Addon bez zadnych uprawnien

        // Arrange
        let state = create_test_addon_state("empty-addon", vec![], None, true);

        // Act & Assert
        assert!(!check_permission(&state, "service", None));
        assert!(!check_permission(&state, "llm", None));
        assert!(!check_permission(&state, "storage", None));
    }
}
