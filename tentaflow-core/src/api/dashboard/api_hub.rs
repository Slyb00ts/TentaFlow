// =============================================================================
// Plik: api/dashboard/api_hub.rs
// Opis: REST endpointy Hub — rejestr silnikow, wyszukiwanie modeli HF,
//       zarzadzanie lokalnymi modelami, pobieranie z postepu.
// =============================================================================

use crate::hub::{engine_registry, hf_search, model_store};
use crate::mesh::peer_store::MeshPeerStore;

/// GET /api/hub/engines — lista silnikow (opcjonalnie filtrowana po os_info, node_id lub type)
pub fn handle_list_engines(query: &str, mesh_peer_store: &MeshPeerStore) -> Result<String, String> {
    let os_info = parse_query_value(query, "os_info");
    let node_id_param = parse_query_value(query, "node_id");
    let type_filter = parse_query_value(query, "type");

    let platform = if let Some(ref os) = os_info {
        engine_registry::parse_platform(os)
    } else if let Some(ref nid) = node_id_param {
        // Sprawdz platforme peera z mesh
        // Frontend przekazuje node_id w formacie "mesh-{node_id}" — stripuj prefix
        let node_id = nid.strip_prefix("mesh-").unwrap_or(nid);
        let peers = mesh_peer_store.list();
        let peer = peers.iter().find(|p| {
            p.node_id == node_id || p.hostname == node_id || p.node_id == *nid || p.hostname == *nid
        });

        if let Some(peer) = peer {
            // Rola "mobile" oznacza iOS/Android — System::name() zwraca "Darwin" na obu
            // platformach Apple, wiec nie mozna polegac tylko na os_info
            if peer.role == "mobile" {
                engine_registry::Platform::IOS
            } else {
                engine_registry::parse_platform(&peer.os_info)
            }
        } else {
            engine_registry::current_platform()
        }
    } else {
        // Brak filtru — uzyj biezacej platformy hosta
        engine_registry::current_platform()
    };

    let engines: Vec<_> = if let Some(ref et) = type_filter {
        engine_registry::engines_for_platform_and_type(&platform, et)
            .iter()
            .map(|e| e.to_info(&platform))
            .collect()
    } else {
        engine_registry::engines_for_platform(&platform)
            .iter()
            .map(|e| e.to_info(&platform))
            .collect()
    };

    serde_json::to_string(&engines).map_err(|e| format!("Serializacja: {}", e))
}

/// GET /api/hub/models/search?q=...&engine=...&limit=20 — wyszukiwanie modeli HF
pub async fn handle_search_models(query: &str) -> Result<String, String> {
    let q = parse_query_value(query, "q").unwrap_or_default();
    let engine = parse_query_value(query, "engine").unwrap_or_else(|| "sglang".to_string());
    let limit: u32 = parse_query_value(query, "limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

    let results = hf_search::search_models(&q, &engine, limit).await?;
    serde_json::to_string(&results).map_err(|e| format!("Serializacja: {}", e))
}

/// GET /api/hub/models/defaults?engine=... — domyslne modele per silnik
pub fn handle_default_models(query: &str) -> Result<String, String> {
    let engine = parse_query_value(query, "engine").unwrap_or_else(|| "sglang".to_string());
    let models = hf_search::default_models(&engine);
    serde_json::to_string(&models).map_err(|e| format!("Serializacja: {}", e))
}

/// GET /api/hub/models/local — lista pobranych modeli
pub fn handle_list_local_models() -> Result<String, String> {
    let store = model_store::ModelStore::default_for_platform();
    let models = store.list_models();
    serde_json::to_string(&models).map_err(|e| format!("Serializacja: {}", e))
}

/// POST /api/hub/models/download — rozpocznij pobieranie modelu
pub async fn handle_download_model(body: &[u8]) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct DownloadReq {
        model_id: String,
        #[serde(default)]
        engine_id: String,
        hf_token: Option<String>,
    }

    let req: DownloadReq =
        serde_json::from_slice(body).map_err(|e| format!("Blad parsowania: {}", e))?;

    let store = model_store::ModelStore::default_for_platform();

    // Sprawdz czy juz pobrany
    let engine = engine_registry::engine_by_id(&req.engine_id);
    let format = engine.map(|e| e.model_format).unwrap_or("safetensors");

    if store.is_downloaded(&req.model_id, format) {
        return Ok(serde_json::json!({
            "ok": true,
            "message": "Model juz pobrany",
            "path": store.model_dir(&req.model_id).to_string_lossy()
        })
        .to_string());
    }

    // Pobierz w tle
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(32);
    let model_id = req.model_id.clone();
    let hf_token = req.hf_token.clone();

    tokio::spawn(async move {
        let store = model_store::ModelStore::default_for_platform();
        match store
            .download_model(&model_id, hf_token.as_deref(), progress_tx)
            .await
        {
            Ok(path) => {
                tracing::info!(model_id = %model_id, path = %path.display(), "Model pobrany");
            }
            Err(e) => {
                tracing::error!(model_id = %model_id, error = %e, "Blad pobierania modelu");
            }
        }
    });

    // Konsumuj progress w tle (zeby kanal sie nie zabloklowal)
    tokio::spawn(async move {
        while let Some(_p) = progress_rx.recv().await {
            // W przyszlosci: SSE stream lub broadcast
        }
    });

    Ok(serde_json::json!({
        "ok": true,
        "message": "Pobieranie rozpoczete",
        "model_id": req.model_id
    })
    .to_string())
}

/// DELETE /api/hub/models/local/{model_id} — usun pobrany model
pub fn handle_delete_local_model(model_id: &str) -> Result<String, String> {
    let store = model_store::ModelStore::default_for_platform();
    store.delete_model(model_id)?;
    Ok(serde_json::json!({"ok": true}).to_string())
}

/// Parsuje wartosc parametru z query string
fn parse_query_value(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        let val = parts.next()?;
        if key == name {
            Some(urlencoding::decode(val).unwrap_or_default().to_string())
        } else {
            None
        }
    })
}
