// =============================================================================
// Plik: flow_engine/dispatchers_impl/vision_impl.rs
// Opis: VisionDispatcherImpl — backuje węzły vision flow przez
//       `ModelRuntimeExecutor::execute_camera_cv` (FAZA 4): alias z requestu
//       jest resolvowany przez katalog (`ServiceSurface::CameraCv`) z pełnym
//       failover/mesh-forward. Gdy slot executora jest pusty (bootstrap/testy),
//       spada na bezpośrednią ścieżkę do procesowych singletonów Burn (te same
//       modele co zawsze-włączony silnik kamer — brak drugiej kopii na GPU).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::sync::Arc;

use super::ModelRuntimeSlot;
use crate::flow_engine::dispatcher::CallProvenance;
use crate::flow_engine::dispatchers::{VisionClassifyRequest, VisionDispatcher, VisionOcrRequest};
use crate::services::runtime::context::ExecutionContext as RuntimeContext;
use crate::services::runtime::executor::ModelRuntimeExecutor;
use crate::services::runtime::local_cv::{CameraCvOpLocal, CameraCvRequest, CvFrameLocal};
use tentaflow_protocol::{CameraCvResult, CvOcrMode};

/// Domyślne aliasy katalogowe gdy request nie wskazuje modelu (seed
/// `seed_camera_cv_aliases` gwarantuje ich istnienie).
const DEFAULT_OCR_ALIAS: &str = "tentavision-ocr";
const DEFAULT_CLASSIFY_ALIAS: &str = "tentavision-stan";

pub struct VisionDispatcherImpl {
    runtime: ModelRuntimeSlot,
}

impl VisionDispatcherImpl {
    pub fn new(runtime: ModelRuntimeSlot) -> Self {
        Self { runtime }
    }

    /// Leniwie czyta slot executora. `None` = bootstrap/testy przed wpięciem
    /// executora — caller spada wtedy na bezpośrednią ścieżkę singletonów.
    fn runtime(&self) -> Option<Arc<ModelRuntimeExecutor>> {
        self.runtime.read().as_ref().cloned()
    }
}

/// Buduje zero-copy klatkę lokalnego surface'u CameraCv z pól requestu
/// dispatchera (RGB24, stride = width * 3).
fn frame_from(rgb: Vec<u8>, width: u32, height: u32) -> CvFrameLocal {
    CvFrameLocal {
        data: Arc::from(rgb),
        width,
        height,
    }
}

/// Kontekst runtime dla wywołania CV z flow — węzły vision nie niosą
/// tożsamości użytkownika, ale caller addon przechodzi do resolvera
/// (widoczność aliasów / permission gating).
fn runtime_ctx(caller_addon_id: Option<String>, provenance: CallProvenance) -> RuntimeContext {
    let mut ctx = RuntimeContext::new(None, provenance.origin, provenance.actor);
    ctx.addon_id = caller_addon_id;
    ctx
}

/// OCR przez executor: alias → resolve → local/mesh dispatch. Tryb `Plate`
/// jest jedynym wariantem osiągalnym z flow — `VisionOcrRequest` nie niesie
/// klasy tablicy (ADR ma dedykowaną ścieżkę w silniku kamer).
async fn ocr_via_executor(
    executor: Arc<ModelRuntimeExecutor>,
    req: VisionOcrRequest,
) -> Result<Option<String>> {
    let model = if req.alias.is_empty() {
        DEFAULT_OCR_ALIAS.to_string()
    } else {
        req.alias.clone()
    };
    let cv_req = CameraCvRequest {
        model,
        op: CameraCvOpLocal::Ocr {
            crop: frame_from(req.rgb, req.width, req.height),
            mode: CvOcrMode::Plate,
        },
    };
    let mut ctx = runtime_ctx(req.caller_addon_id, req.provenance);
    match executor.execute_camera_cv(cv_req, &mut ctx).await {
        Ok(CameraCvResult::Text { tekst }) => Ok(tekst),
        Ok(_) => Err(anyhow!(
            "vision ocr: nieoczekiwany wariant wyniku camera-cv"
        )),
        Err(e) => Err(anyhow!("vision ocr: {e}")),
    }
}

/// Klasyfikacja stanu przez executor — lustro `ocr_via_executor`.
async fn classify_via_executor(
    executor: Arc<ModelRuntimeExecutor>,
    req: VisionClassifyRequest,
) -> Result<Vec<String>> {
    let model = if req.alias.is_empty() {
        DEFAULT_CLASSIFY_ALIAS.to_string()
    } else {
        req.alias.clone()
    };
    let cv_req = CameraCvRequest {
        model,
        op: CameraCvOpLocal::ClassifyState {
            crop: frame_from(req.rgb, req.width, req.height),
        },
    };
    let mut ctx = runtime_ctx(req.caller_addon_id, req.provenance);
    match executor.execute_camera_cv(cv_req, &mut ctx).await {
        Ok(CameraCvResult::Labels { stan }) => Ok(stan),
        Ok(_) => Err(anyhow!(
            "vision classify: nieoczekiwany wariant wyniku camera-cv"
        )),
        Err(e) => Err(anyhow!("vision classify: {e}")),
    }
}

/// Próbuje rozpoznac tekst przez nadpisany in-process OCR runner (np.
/// `apple-ocr`). Zwraca `Some(result)` gdy runner jest ustawiony (i jego wynik),
/// `None` gdy zaden override nie jest zarejestrowany — caller spada wtedy na
/// wbudowany Burn `PlateOcr`. Dziala niezaleznie od `inference-vision-gpu`.
async fn try_override_ocr(req: &VisionOcrRequest) -> Option<Result<Option<String>>> {
    let runner = crate::vision::get_ocr_runner()?;
    let rgb = req.rgb.clone();
    let (width, height) = (req.width, req.height);
    let out = tokio::task::spawn_blocking(move || runner.read(&rgb, width, height))
        .await
        .map_err(|e| anyhow::anyhow!("vision ocr task: {e}"))
        .and_then(|r| r);
    Some(out)
}

/// Bezpośrednia ścieżka OCR (fallback gdy slot executora pusty): nadpisany
/// runner ma pierwszeństwo, potem wbudowany Burn `PlateOcr`.
#[cfg(feature = "inference-vision-gpu")]
async fn ocr_direct(req: VisionOcrRequest) -> Result<Option<String>> {
    if let Some(out) = try_override_ocr(&req).await {
        return out;
    }
    let Some(runner) = crate::vision::runners::get_ocr().await else {
        return Ok(None); // runner niedostępny → brak tekstu, nigdy błąd
    };
    let VisionOcrRequest {
        rgb, width, height, ..
    } = req;
    // ort: pulowany `&self` runner (spawn_blocking, poza wątkiem Burn/wgpu);
    // Burn: `Arc<Mutex<_>>` — zablokuj przed forwardem.
    #[cfg(feature = "vision-ort")]
    let out = tokio::task::spawn_blocking(move || runner.read(&rgb, width, height))
        .await
        .map_err(|e| anyhow::anyhow!("vision ocr task: {e}"))??;
    #[cfg(not(feature = "vision-ort"))]
    let out = tokio::task::spawn_blocking(move || runner.lock().unwrap().read(&rgb, width, height))
        .await
        .map_err(|e| anyhow::anyhow!("vision ocr task: {e}"))??;
    Ok(out)
}

/// Bez Burn `PlateOcr` (brak `inference-vision-gpu`) jedyna sciezka OCR to
/// nadpisany in-process runner, np. `apple-ocr` na macOS/iOS.
#[cfg(not(feature = "inference-vision-gpu"))]
async fn ocr_direct(req: VisionOcrRequest) -> Result<Option<String>> {
    if let Some(out) = try_override_ocr(&req).await {
        return out;
    }
    Ok(None)
}

/// Bezpośrednia ścieżka klasyfikacji stanu (fallback gdy slot executora pusty).
#[cfg(feature = "inference-vision-gpu")]
async fn classify_direct(req: VisionClassifyRequest) -> Result<Vec<String>> {
    let Some(runner) = crate::vision::runners::get_classifier().await
    else {
        return Ok(Vec::new());
    };
    let VisionClassifyRequest {
        rgb, width, height, ..
    } = req;
    // ort: pulowany `&self` runner (spawn_blocking, poza wątkiem Burn/wgpu);
    // Burn: `Arc<Mutex<_>>` — zablokuj przed forwardem.
    #[cfg(feature = "vision-ort")]
    let out = tokio::task::spawn_blocking(move || runner.classify(&rgb, width, height))
        .await
        .map_err(|e| anyhow::anyhow!("vision classify task: {e}"))??;
    #[cfg(not(feature = "vision-ort"))]
    let out =
        tokio::task::spawn_blocking(move || runner.lock().unwrap().classify(&rgb, width, height))
            .await
            .map_err(|e| anyhow::anyhow!("vision classify task: {e}"))??;
    Ok(out)
}

#[cfg(not(feature = "inference-vision-gpu"))]
async fn classify_direct(_req: VisionClassifyRequest) -> Result<Vec<String>> {
    Ok(Vec::new())
}

#[async_trait]
impl VisionDispatcher for VisionDispatcherImpl {
    async fn ocr(&self, req: VisionOcrRequest) -> Result<Option<String>> {
        if let Some(executor) = self.runtime() {
            return ocr_via_executor(executor, req).await;
        }
        ocr_direct(req).await
    }

    async fn classify(&self, req: VisionClassifyRequest) -> Result<Vec<String>> {
        if let Some(executor) = self.runtime() {
            return classify_via_executor(executor, req).await;
        }
        classify_direct(req).await
    }
}
