// =============================================================================
// Plik: api/dashboard/api_portainer.rs
// Opis: REST API dla Portainer - CRUD instancji, proxy do endpointow/stackow.
// =============================================================================

use crate::services::portainer::{PortainerClient, PortainerConfig, ContainerAction};
use crate::crypto::SecretsCipher;
use crate::db::{self, DbPool};
use anyhow::Context;
use std::sync::Arc;

/// Body requestu deploy stacka
#[derive(serde::Deserialize)]
struct DeployStackRequest {
    name: String,
    compose_content: String,
}

/// Body requestu akcji na kontenerze
#[derive(serde::Deserialize)]
struct ContainerActionRequest {
    action: String,
}

/// Body requestu tworzenia/aktualizacji instancji Portainer
#[derive(serde::Deserialize)]
struct PortainerInstanceRequest {
    name: String,
    url: String,
    api_key: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

/// Tworzy klienta Portainer z instancji w bazie danych.
async fn create_portainer_client(db: &DbPool, cipher: &Arc<SecretsCipher>, instance_id: i64) -> anyhow::Result<PortainerClient> {
    let instance = db::repository::get_portainer_instance(db, instance_id)?
        .ok_or_else(|| anyhow::anyhow!("Instancja Portainer o id {} nie istnieje", instance_id))?;

    let decrypted_api_key = cipher.decrypt_if_encrypted(&instance.api_key).into_owned();
    let decrypted_password = cipher.decrypt_if_encrypted(&instance.password).into_owned();

    let api_key = if !decrypted_api_key.is_empty() {
        decrypted_api_key
    } else if !instance.username.is_empty() && !decrypted_password.is_empty() {
        authenticate_portainer(&instance.url, &instance.username, &decrypted_password).await?
    } else {
        anyhow::bail!("Brak danych uwierzytelniajacych - podaj API key lub login/haslo")
    };

    PortainerClient::new(PortainerConfig {
        base_url: instance.url,
        api_key,
        display_name: instance.name,
    })
}

/// Loguje sie do Portainer przez POST /api/auth i zwraca JWT token.
async fn authenticate_portainer(base_url: &str, username: &str, password: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let url = format!("{}/api/auth", base_url.trim_end_matches('/'));
    let body = serde_json::json!({ "username": username, "password": password });

    let response = client.post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .with_context(|| format!("Blad polaczenia z Portainer auth: {}", url))?;

    if !response.status().is_success() {
        let status = response.status();
        let error_body = response.text().await.unwrap_or_default();
        anyhow::bail!("Blad logowania do Portainer ({}): {}", status, error_body);
    }

    #[derive(serde::Deserialize)]
    struct AuthResponse { jwt: String }

    let auth: AuthResponse = response.json().await
        .context("Niepoprawna odpowiedz z Portainer auth")?;
    Ok(auth.jwt)
}

// --- CRUD instancji Portainer ---

/// GET /api/portainer-instances - lista instancji (api_key maskowany)
pub fn handle_list_instances(db: &DbPool) -> anyhow::Result<(u16, String)> {
    let instances = db::repository::list_portainer_instances(db)?;
    let masked: Vec<serde_json::Value> = instances.iter().map(|inst| {
        serde_json::json!({
            "id": inst.id,
            "name": inst.name,
            "url": inst.url,
            "api_key": mask_api_key(&inst.api_key),
            "username": inst.username,
            "created_at": inst.created_at,
            "updated_at": inst.updated_at,
        })
    }).collect();
    Ok((200, serde_json::to_string(&masked)?))
}

/// POST /api/portainer-instances - dodaj instancje
pub fn handle_create_instance(db: &DbPool, cipher: &Arc<SecretsCipher>, body: &[u8]) -> anyhow::Result<(u16, String)> {
    let req: PortainerInstanceRequest = serde_json::from_slice(body)?;
    let encrypted_api_key = if req.api_key.is_empty() {
        String::new()
    } else {
        cipher.encrypt(&req.api_key)?
    };
    let encrypted_password = if req.password.is_empty() {
        String::new()
    } else {
        cipher.encrypt(&req.password)?
    };
    let id = db::repository::create_portainer_instance(db, &req.name, &req.url, &encrypted_api_key, &req.username, &encrypted_password)?;
    Ok((201, serde_json::json!({"id": id}).to_string()))
}

/// PUT /api/portainer-instances/:id - aktualizuj instancje
pub fn handle_update_instance(db: &DbPool, cipher: &Arc<SecretsCipher>, id: i64, body: &[u8]) -> anyhow::Result<(u16, String)> {
    let req: PortainerInstanceRequest = serde_json::from_slice(body)?;
    let encrypted_api_key = if req.api_key.is_empty() {
        match db::repository::get_portainer_instance(db, id)? {
            Some(existing) => existing.api_key,
            None => return Ok((404, serde_json::json!({"error": "Instancja nie znaleziona"}).to_string())),
        }
    } else {
        cipher.encrypt(&req.api_key)?
    };
    let encrypted_password = if req.password.is_empty() {
        match db::repository::get_portainer_instance(db, id)? {
            Some(existing) => existing.password,
            None => return Ok((404, serde_json::json!({"error": "Instancja nie znaleziona"}).to_string())),
        }
    } else {
        cipher.encrypt(&req.password)?
    };
    db::repository::update_portainer_instance(db, id, &req.name, &req.url, &encrypted_api_key, &req.username, &encrypted_password)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

/// DELETE /api/portainer-instances/:id - usun instancje
pub fn handle_delete_instance(db: &DbPool, id: i64) -> anyhow::Result<(u16, String)> {
    db::repository::delete_portainer_instance(db, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

/// Maskuje api_key do formatu "ptr_***"
fn mask_api_key(key: &str) -> String {
    if key.len() > 6 {
        format!("{}***", &key[..6])
    } else {
        "***".to_string()
    }
}

// --- Proxy do Portainer API (per instancja) ---

/// GET /api/portainer/instances/:iid/status - test polaczenia
pub async fn handle_status(db: &DbPool, cipher: &Arc<SecretsCipher>, instance_id: i64) -> (u16, String) {
    let client = match create_portainer_client(db, cipher, instance_id).await {
        Ok(c) => c,
        Err(e) => return (400, serde_json::json!({"error": e.to_string()}).to_string()),
    };
    match client.list_endpoints().await {
        Ok(endpoints) => (200, serde_json::json!({"connected": true, "endpoint_count": endpoints.len()}).to_string()),
        Err(e) => (502, serde_json::json!({"connected": false, "error": e.to_string()}).to_string()),
    }
}

/// GET /api/portainer/instances/:iid/endpoints - lista endpointow
pub async fn handle_list_endpoints(db: &DbPool, cipher: &Arc<SecretsCipher>, instance_id: i64) -> (u16, String) {
    let client = match create_portainer_client(db, cipher, instance_id).await {
        Ok(c) => c,
        Err(e) => return (400, serde_json::json!({"error": e.to_string()}).to_string()),
    };
    match client.list_endpoints().await {
        Ok(endpoints) => match serde_json::to_string(&endpoints) {
            Ok(json) => (200, json),
            Err(e) => (500, serde_json::json!({"error": format!("Blad serializacji: {}", e)}).to_string()),
        },
        Err(e) => (502, serde_json::json!({"error": e.to_string()}).to_string()),
    }
}

/// GET /api/portainer/instances/:iid/endpoints/:eid/containers - kontenery na endpoincie
pub async fn handle_list_containers(db: &DbPool, cipher: &Arc<SecretsCipher>, instance_id: i64, endpoint_id: i64) -> (u16, String) {
    let client = match create_portainer_client(db, cipher, instance_id).await {
        Ok(c) => c,
        Err(e) => return (400, serde_json::json!({"error": e.to_string()}).to_string()),
    };
    match client.list_containers(endpoint_id).await {
        Ok(containers) => match serde_json::to_string(&containers) {
            Ok(json) => (200, json),
            Err(e) => (500, serde_json::json!({"error": format!("Blad serializacji: {}", e)}).to_string()),
        },
        Err(e) => (502, serde_json::json!({"error": e.to_string()}).to_string()),
    }
}

/// GET /api/portainer/instances/:iid/endpoints/:eid/stacks - stacki na endpoincie
pub async fn handle_list_stacks(db: &DbPool, cipher: &Arc<SecretsCipher>, instance_id: i64, endpoint_id: i64) -> (u16, String) {
    let client = match create_portainer_client(db, cipher, instance_id).await {
        Ok(c) => c,
        Err(e) => return (400, serde_json::json!({"error": e.to_string()}).to_string()),
    };
    match client.list_stacks(endpoint_id).await {
        Ok(stacks) => match serde_json::to_string(&stacks) {
            Ok(json) => (200, json),
            Err(e) => (500, serde_json::json!({"error": format!("Blad serializacji: {}", e)}).to_string()),
        },
        Err(e) => (502, serde_json::json!({"error": e.to_string()}).to_string()),
    }
}

/// POST /api/portainer/instances/:iid/endpoints/:eid/stacks - deploy stack
pub async fn handle_deploy_stack(db: &DbPool, cipher: &Arc<SecretsCipher>, instance_id: i64, endpoint_id: i64, body: &[u8]) -> (u16, String) {
    let req: DeployStackRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return (400, serde_json::json!({"error": format!("Niepoprawny JSON: {}", e)}).to_string()),
    };
    let client = match create_portainer_client(db, cipher, instance_id).await {
        Ok(c) => c,
        Err(e) => return (400, serde_json::json!({"error": e.to_string()}).to_string()),
    };
    match client.deploy_stack(endpoint_id, &req.name, &req.compose_content).await {
        Ok(stack) => match serde_json::to_string(&stack) {
            Ok(json) => (200, json),
            Err(e) => (500, serde_json::json!({"error": format!("Blad serializacji: {}", e)}).to_string()),
        },
        Err(e) => (502, serde_json::json!({"error": e.to_string()}).to_string()),
    }
}

/// DELETE /api/portainer/instances/:iid/stacks/:sid - usun stack
pub async fn handle_remove_stack(db: &DbPool, cipher: &Arc<SecretsCipher>, instance_id: i64, stack_id: i64, query: &str) -> (u16, String) {
    let endpoint_id: i64 = query
        .split('&')
        .find_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?;
            let val = parts.next()?;
            if key == "endpoint_id" { val.parse().ok() } else { None }
        })
        .unwrap_or(0);

    if endpoint_id == 0 {
        return (400, r#"{"error":"Brak parametru endpoint_id"}"#.to_string());
    }

    let client = match create_portainer_client(db, cipher, instance_id).await {
        Ok(c) => c,
        Err(e) => return (400, serde_json::json!({"error": e.to_string()}).to_string()),
    };
    match client.remove_stack(stack_id, endpoint_id).await {
        Ok(()) => (200, r#"{"ok":true}"#.to_string()),
        Err(e) => (502, serde_json::json!({"error": e.to_string()}).to_string()),
    }
}

/// POST /api/portainer/instances/:iid/endpoints/:eid/containers/:cid/action - akcja na kontenerze
pub async fn handle_container_action(
    db: &DbPool,
    cipher: &Arc<SecretsCipher>,
    instance_id: i64,
    endpoint_id: i64,
    container_id: &str,
    body: &[u8],
) -> (u16, String) {
    let req: ContainerActionRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return (400, serde_json::json!({"error": format!("Niepoprawny JSON: {}", e)}).to_string()),
    };
    let action = match req.action.as_str() {
        "start" => ContainerAction::Start,
        "stop" => ContainerAction::Stop,
        "restart" => ContainerAction::Restart,
        "kill" => ContainerAction::Kill,
        other => return (400, serde_json::json!({"error": format!("Nieznana akcja: {}", other)}).to_string()),
    };
    let client = match create_portainer_client(db, cipher, instance_id).await {
        Ok(c) => c,
        Err(e) => return (400, serde_json::json!({"error": e.to_string()}).to_string()),
    };
    match client.container_action(endpoint_id, container_id, action).await {
        Ok(()) => (200, r#"{"ok":true}"#.to_string()),
        Err(e) => (502, serde_json::json!({"error": e.to_string()}).to_string()),
    }
}

/// GET /api/portainer/instances/:iid/endpoints/:eid/containers/:cid/logs - logi kontenera
pub async fn handle_container_logs(
    db: &DbPool,
    cipher: &Arc<SecretsCipher>,
    instance_id: i64,
    endpoint_id: i64,
    container_id: &str,
) -> (u16, String) {
    let client = match create_portainer_client(db, cipher, instance_id).await {
        Ok(c) => c,
        Err(e) => return (400, serde_json::json!({"error": e.to_string()}).to_string()),
    };
    match client.container_logs(endpoint_id, container_id, 100).await {
        Ok(logs) => match serde_json::to_string(&logs) {
            Ok(json) => (200, json),
            Err(e) => (500, serde_json::json!({"error": format!("Blad serializacji: {}", e)}).to_string()),
        },
        Err(e) => (502, serde_json::json!({"error": e.to_string()}).to_string()),
    }
}
