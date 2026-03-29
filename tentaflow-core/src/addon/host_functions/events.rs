// =============================================================================
// Plik: addon/host_functions/events.rs
// Opis: Host functions Event API — subskrypcja i publikacja eventow.
//       Addon subskrybuje eventy; Core wywola guest export on_event() przy dostarczeniu.
// =============================================================================

use tracing::info;

use super::{
    AddonState, ABI_OK, ABI_ERR_PERMISSION, ABI_ERR_OPERATION,
    get_memory, read_guest_string, audit_log, check_permission,
    WasmCaller,
};
use crate::addon::event_bus::EventSubscriber;

// =============================================================================
// event_subscribe — subskrypcja eventu
// =============================================================================

/// Host function: subskrybuje typ eventu.
/// Core wywola guest export `on_event(event_json_ptr, event_json_len)` przy dostarczeniu.
///
/// ABI:
/// - event_type_ptr/event_type_len: typ eventu (np. "message_received")
/// - filter_json_ptr/filter_json_len: opcjonalny filtr JSON (0,0 = brak)
/// - Zwraca: subscription_id (>0) lub blad (<0)
pub fn event_subscribe(
    mut caller: WasmCaller<'_, AddonState>,
    event_type_ptr: i32,
    event_type_len: i32,
    filter_json_ptr: i32,
    filter_json_len: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let event_type = match read_guest_string(&memory, &caller, event_type_ptr, event_type_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    // Odczytaj opcjonalny filtr
    let _filter = if filter_json_ptr != 0 && filter_json_len > 0 {
        read_guest_string(&memory, &caller, filter_json_ptr, filter_json_len)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
    } else {
        None
    };

    // Sprawdz uprawnienie events z wzorcem event_type
    if !check_permission(caller.data(), "events", Some(&event_type)) {
        audit_log(
            caller.data(),
            "event.subscribe",
            Some("events"),
            Some(&event_type),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();
    let instance_id = caller.data().instance_id.clone();

    info!("event_subscribe: addon='{}', event_type='{}'", addon_id, event_type);

    // Zarejestruj subskrypcje w event bus
    let subscriber = EventSubscriber {
        addon_id: addon_id.clone(),
        instance_id: instance_id.clone(),
        callback_name: "on_event".to_string(),
    };

    let subscription_id = caller.data().event_bus.subscribe(&event_type, subscriber);

    // Zapisz subskrypcje w DB
    {
        match caller.data().db.lock() {
            Ok(conn) => {
                let filter_str = _filter.as_ref().map(|f| f.to_string());
                let _ = conn.execute(
                    "INSERT OR REPLACE INTO addon_event_subscriptions \
                     (addon_id, instance_id, event_type, event_filter_json, is_active) \
                     VALUES (?1, ?2, ?3, ?4, 1)",
                    rusqlite::params![&addon_id, &instance_id, &event_type, &filter_str],
                );
            }
            Err(_) => return ABI_ERR_OPERATION,
        }
    }

    audit_log(
        caller.data(),
        "event.subscribe",
        Some("events"),
        Some(&event_type),
        "ok",
        None,
    );

    subscription_id as i32
}

// =============================================================================
// event_publish — publikacja eventu
// =============================================================================

/// Host function: publikuje event na bus.
/// Wymaga uprawnienia 'events' z access_level 'rw'.
///
/// ABI:
/// - event_type_ptr/event_type_len: typ eventu
/// - payload_json_ptr/payload_json_len: payload JSON
/// - Zwraca: ABI_OK lub kod bledu
pub fn event_publish(
    mut caller: WasmCaller<'_, AddonState>,
    event_type_ptr: i32,
    event_type_len: i32,
    payload_json_ptr: i32,
    payload_json_len: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let event_type = match read_guest_string(&memory, &caller, event_type_ptr, event_type_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    let payload = if payload_json_ptr != 0 && payload_json_len > 0 {
        match read_guest_string(&memory, &caller, payload_json_ptr, payload_json_len) {
            Some(s) => match serde_json::from_str::<serde_json::Value>(s) {
                Ok(v) => v,
                Err(_) => return ABI_ERR_OPERATION,
            },
            None => return ABI_ERR_OPERATION,
        }
    } else {
        serde_json::Value::Null
    };

    // Sprawdz uprawnienie events (rw — publikacja wymaga write)
    if !check_permission(caller.data(), "events", Some(&event_type)) {
        audit_log(
            caller.data(),
            "event.publish",
            Some("events"),
            Some(&event_type),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();
    let user_id = caller.data().user_id;

    info!("event_publish: addon='{}', event_type='{}'", addon_id, event_type);

    // Opublikuj event
    let event = crate::addon::event_bus::Event {
        event_type: event_type.clone(),
        source_addon: Some(addon_id.clone()),
        source_user: user_id,
        payload,
        timestamp: chrono::Utc::now(),
    };

    caller.data().event_bus.publish(event);

    audit_log(
        caller.data(),
        "event.publish",
        Some("events"),
        Some(&event_type),
        "ok",
        None,
    );

    ABI_OK
}
