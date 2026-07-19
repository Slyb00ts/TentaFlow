// ===== File: dispatch/vision_import.rs — custom vision-model import handlers =====
//
// Deploy-wizard "Własny (URL + klucz API)" tab: an admin/PowerUser on an
// UNPAIRED node pastes another instance's `/models/manifest/<ref>` URL plus an
// API key, previews the models, and imports one into the local `vision_models`
// registry. Core (never the browser) performs the HTTPS pull through the same
// no-redirect / query-redacting client the camera-CV bundle path uses.

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, ProtocolError, ProtocolErrorCode, VisionImportFetchManifestResponse,
    VisionImportManifestFile, VisionImportManifestModel, VisionImportModelResponse,
    VisionImportPayload,
};

use super::HandlerContext;
use crate::services::rbac::OrgContext;

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

#[handler(variant = "VisionImportFetchManifestRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn vision_import_fetch_manifest(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::VisionImportBody(VisionImportPayload::FetchManifestRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected VisionImportFetchManifestRequest",
            ))
        }
    };

    let respond = |resp: VisionImportFetchManifestResponse| {
        Ok(MessageBody::VisionImportBody(
            VisionImportPayload::FetchManifestResponse(resp),
        ))
    };

    let manifest = match crate::vision::camera_cv_models::fetch_custom_manifest_json(
        &payload.manifest_url,
        &payload.api_key,
    )
    .await
    {
        Ok(m) => m,
        Err(e) => {
            return respond(VisionImportFetchManifestResponse {
                bundle: String::new(),
                files: Vec::new(),
                model: None,
                error: Some(format!("{e:#}")),
            })
        }
    };

    let bundle = manifest
        .get("bundle")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let files = manifest
        .get("files")
        .and_then(|f| f.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| {
                    Some(VisionImportManifestFile {
                        name: e.get("name")?.as_str()?.to_string(),
                        size: e.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                        sha256: e.get("sha256")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // A single-model registry bundle carries a `model` object; a fixed engine
    // bundle does not (its runner is compiled in — nothing to import).
    let model = manifest.get("model").and_then(|m| m.as_object()).map(|m| {
        let classes: Vec<String> = m
            .get("classes_json")
            .and_then(|c| c.as_str())
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default();
        VisionImportManifestModel {
            model_name: m
                .get("model_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            op: m.get("op").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            file_name: m
                .get("file_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            classes,
            output_contract: m
                .get("output_contract")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            default_threshold: m.get("default_threshold").and_then(|v| v.as_f64()),
        }
    });

    respond(VisionImportFetchManifestResponse {
        bundle,
        files,
        model,
        error: None,
    })
}

#[handler(variant = "VisionImportModelRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn vision_import_model(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::VisionImportBody(VisionImportPayload::ImportRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected VisionImportModelRequest")),
    };
    let org = require_org(ctx)?;

    let respond = |ok: bool, imported_model_name: Option<String>, error: Option<String>| {
        Ok(MessageBody::VisionImportBody(
            VisionImportPayload::ImportResponse(VisionImportModelResponse {
                ok,
                imported_model_name,
                error,
            }),
        ))
    };

    // Validate the target name up front (it becomes the registry model name and
    // an on-disk file name) so a bad name never reaches the download path.
    if let Err(e) = crate::db::repository::validate_vision_model_name(&payload.model_name) {
        return respond(false, None, Some(e));
    }

    let import = match crate::vision::camera_cv_models::import_custom_model(
        &payload.manifest_url,
        &payload.api_key,
        &payload.model_name,
        None,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return respond(false, None, Some(format!("{e:#}"))),
    };

    let alias = payload
        .alias
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let row = crate::db::repository::VisionModelRow {
        model_name: import.model_name.clone(),
        op: import.op.clone(),
        file_name: import.file_name.clone(),
        sha256: String::new(),
        classes_json: import.classes_json.clone(),
        preprocess_json: import.preprocess_json.clone(),
        output_contract: import.output_contract.clone(),
        source: "imported".to_string(),
        default_threshold: import.default_threshold,
        org_id: org.org_id.clone(),
        project_id: None,
        source_model_id: None,
        created_at: 0,
        updated_at: 0,
    };
    // The registry row stores the ONNX sha256; recompute it from the file we
    // just verified against the manifest so the row is self-consistent even if
    // the remote manifest and the local file ever diverged.
    let onnx_path = crate::paths::vision_models_dir().join(&import.file_name);
    let sha256 = match crate::api::model_bundle::sha256_file_hex(&onnx_path).await {
        Ok(h) => h,
        Err(e) => {
            for f in &import.written_files {
                let _ = std::fs::remove_file(f);
            }
            return respond(false, None, Some(format!("hash pobranego ONNX: {e}")));
        }
    };
    let row = crate::db::repository::VisionModelRow { sha256, ..row };

    if let Err(e) = crate::db::repository::register_vision_model(&ctx.state.db, &row, alias) {
        // Registration refused — remove the files we wrote so a failed import
        // leaves nothing orphaned in vision_models_dir().
        for f in &import.written_files {
            let _ = std::fs::remove_file(f);
        }
        return respond(false, None, Some(e.to_string()));
    }

    if alias.is_some() {
        crate::services::models::broadcast_alias_mutation(
            &ctx.state.db,
            &ctx.state.router,
            &ctx.state.quic_mesh,
        );
    }
    crate::services::onnx_cv_service::reconcile_and_announce(&ctx.state);
    respond(true, Some(import.model_name), None)
}
