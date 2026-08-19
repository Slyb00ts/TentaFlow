// =============================================================================
// Plik: addon/host_functions/service.rs
// Opis: Cienki wrapper WASM-ABI nad `services::service_call::dispatch`.
//       Czyta argumenty z liniowej pamieci WASM, woła pure-async dispatch
//       (permissions, rate limit, alias gate, pickup token, QUIC) na biezacym
//       runtime tokio, zapisuje odpowiedz do guest output bufera. Cała logika
//       dispatchu zyje w `services::service_call` — operatory flow_runtime
//       wolają to samo bez przechodzenia przez WASM.
// =============================================================================

use tracing::warn;

use super::{
    audit_log, get_memory, read_guest_string, write_guest_output, AddonState, WasmCaller,
    ABI_ERR_NOT_FOUND, ABI_ERR_OPERATION, ABI_ERR_PERMISSION, ABI_ERR_RATE_LIMIT,
};

use crate::addon::errors::AbiError;
use crate::addon::rate_limiter::ResourceType;
use crate::services::service_call::{
    dispatch, CallerContext, ServiceCallError, ServiceCallRequest,
};

#[allow(clippy::too_many_arguments)]
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

    let service_name = match read_guest_string(&memory, &caller, service_name_ptr, service_name_len)
    {
        Some(s) => s.to_string(),
        None => {
            warn!("service_request: niepoprawny wskaznik service_name");
            return ABI_ERR_OPERATION;
        }
    };

    let request_json = match read_guest_string(&memory, &caller, request_json_ptr, request_json_len)
    {
        Some(s) => s.to_string(),
        None => {
            warn!("service_request: niepoprawny wskaznik request_json");
            return ABI_ERR_OPERATION;
        }
    };

    let state = caller.data();
    let req = ServiceCallRequest {
        caller: CallerContext {
            addon_id: state.addon_id.clone(),
            user_id: state.user_id.clone(),
            instance_id: Some(state.instance_id.clone()),
            is_system_call: state.is_system_call,
            org_id: state.org_id.clone(),
        },
        service_name: service_name.clone(),
        payload_json: request_json,
        timeout_ms: 0,
        // WASM service_request keeps legacy semantics: service_name may be an
        // alias OR a concrete service; both are accepted. Flow operators that
        // mint alias-gated calls set this to true.
        alias_required: false,
    };
    let db = state.db.clone();
    let permission_checker = state.permission_checker.clone();
    let permissions = state.permissions.clone();
    let service_manager = state.router.as_ref().map(|r| r.service_manager().clone());
    let executor = state.router.as_ref().and_then(|r| r.executor());

    let outcome = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            dispatch(
                req,
                &db,
                service_manager.as_ref(),
                executor.as_ref(),
                Some(&permission_checker),
                &permissions,
            )
            .await
        })
    });

    // Legacy AddonRateLimiter (per-resource HttpRequests quota) is charged
    // ONLY when dispatch has cleared permission + alias gate + service-call
    // limiter — i.e. only on success or on a transport-class failure. Denials
    // do not consume HttpRequests quota (matches pre-C0 behavior where the
    // legacy check ran after permission and alias gates).
    let charge_legacy_quota = matches!(
        outcome,
        Ok(_)
            | Err(ServiceCallError::Timeout { .. })
            | Err(ServiceCallError::PickupTokenInjection(_))
            | Err(ServiceCallError::Internal(_))
            | Err(ServiceCallError::ServiceManagerNotInitialized)
    );
    if charge_legacy_quota {
        if let Some(ref rate_limiter) = caller.data().rate_limiter {
            let addon_id = caller.data().addon_id.clone();
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
    }

    match outcome {
        Ok(resp) => {
            let bytes = resp.response_json.into_bytes();
            write_guest_output(&memory, &mut caller, out_ptr, out_cap, out_len_ptr, &bytes)
        }
        Err(ServiceCallError::Permission { .. })
        | Err(ServiceCallError::AliasPermission { .. }) => ABI_ERR_PERMISSION,
        Err(ServiceCallError::RateLimit { .. }) => AbiError::QuotaExceeded.into(),
        Err(ServiceCallError::NotFound { .. }) => ABI_ERR_NOT_FOUND,
        Err(ServiceCallError::Timeout { .. })
        | Err(ServiceCallError::ServiceManagerNotInitialized)
        | Err(ServiceCallError::PickupTokenInjection(_))
        | Err(ServiceCallError::Internal(_)) => ABI_ERR_OPERATION,
    }
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

    fn create_test_db() -> crate::db::DbPool {
        crate::db::init(Path::new(":memory:")).expect("Nie udalo sie utworzyc test DB")
    }

    fn create_test_addon_state(
        addon_id: &str,
        permissions: Vec<String>,
        user_id: Option<String>,
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
            org_id: None,
            db,
            permissions,
            event_bus,
            permission_checker,
            fuel_consumed: 0,
            is_system_call,
            call_provenance: crate::addon::AddonCallProvenance::addon(),
            rate_limiter: None,
            net_manager: Arc::new(Mutex::new(NetworkConnectionManager::new())),
            settings_cipher,
            manifest: Arc::new(AddonManifest::default()),
            memory_limit: 64 * 1024 * 1024,
            oauth_refresh_guard: std::sync::Arc::new(
                crate::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
            ),
            router: None,
            ui_panels: None,
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
        }
    }

    #[test]
    fn check_permission_no_service_permission_returns_false() {
        let state = create_test_addon_state(
            "sdk-showcase",
            vec!["llm".to_string(), "storage".to_string()],
            None,
            true,
        );
        assert!(!check_permission(&state, "service", None));
    }

    #[test]
    fn check_permission_with_service_permission_system_call_returns_true() {
        let state = create_test_addon_state(
            "teams-bot",
            vec!["service".to_string(), "llm".to_string()],
            None,
            true,
        );
        assert!(check_permission(&state, "service", None));
    }

    #[test]
    fn check_permission_without_system_call_no_user_returns_false() {
        let state =
            create_test_addon_state("untrusted-addon", vec!["service".to_string()], None, false);
        assert!(!check_permission(&state, "service", None));
    }

    #[test]
    fn check_permission_service_with_resource_name_no_permission_returns_false() {
        let state = create_test_addon_state("sdk-showcase", vec!["llm".to_string()], None, true);
        assert!(!check_permission(&state, "service", Some("teams-stt")));
    }

    #[test]
    fn check_permission_empty_permissions_returns_false() {
        let state = create_test_addon_state("empty-addon", vec![], None, true);
        assert!(!check_permission(&state, "service", None));
        assert!(!check_permission(&state, "llm", None));
        assert!(!check_permission(&state, "storage", None));
    }
}
