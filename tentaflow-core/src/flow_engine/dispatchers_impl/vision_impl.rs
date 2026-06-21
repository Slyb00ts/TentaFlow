// =============================================================================
// Plik: flow_engine/dispatchers_impl/vision_impl.rs
// Opis: VisionDispatcherImpl — backs the vision flow nodes with the centralized
//       Burn runners (the SAME singletons the always-on camera engine uses, so
//       there is no second model copy on the GPU). Real impl is gated behind
//       `inference-vision-gpu`; without it a no-op impl keeps ExecutionContext
//       constructible. Alias → model resolution + permission gating land in a
//       later chunk; today each capability maps to its single deployed model.
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

use crate::flow_engine::dispatchers::{VisionClassifyRequest, VisionDispatcher, VisionOcrRequest};

pub struct VisionDispatcherImpl;

impl VisionDispatcherImpl {
    pub fn new() -> Self {
        Self
    }
}
impl Default for VisionDispatcherImpl {
    fn default() -> Self {
        Self::new()
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

#[cfg(feature = "inference-vision-gpu")]
#[async_trait]
impl VisionDispatcher for VisionDispatcherImpl {
    async fn ocr(&self, req: VisionOcrRequest) -> Result<Option<String>> {
        // Nadpisany runner (apple-ocr) ma pierwszenstwo nad wbudowanym Burn.
        if let Some(out) = try_override_ocr(&req).await {
            return out;
        }
        let Some(runner) = crate::services::camera_ingest::vision_analysis::get_ocr().await else {
            return Ok(None); // runner unavailable → no text, never an error
        };
        let VisionOcrRequest {
            rgb, width, height, ..
        } = req;
        let out = tokio::task::spawn_blocking(move || runner.lock().unwrap().read(&rgb, width, height))
            .await
            .map_err(|e| anyhow::anyhow!("vision ocr task: {e}"))??;
        Ok(out)
    }

    async fn classify(&self, req: VisionClassifyRequest) -> Result<Vec<String>> {
        let Some(runner) = crate::services::camera_ingest::vision_analysis::get_classifier().await
        else {
            return Ok(Vec::new());
        };
        let VisionClassifyRequest {
            rgb, width, height, ..
        } = req;
        let out =
            tokio::task::spawn_blocking(move || runner.lock().unwrap().classify(&rgb, width, height))
                .await
                .map_err(|e| anyhow::anyhow!("vision classify task: {e}"))??;
        Ok(out)
    }
}

#[cfg(not(feature = "inference-vision-gpu"))]
#[async_trait]
impl VisionDispatcher for VisionDispatcherImpl {
    async fn ocr(&self, req: VisionOcrRequest) -> Result<Option<String>> {
        // Bez Burn `PlateOcr` (brak `inference-vision-gpu`) jedyna sciezka OCR to
        // nadpisany in-process runner, np. `apple-ocr` na macOS/iOS.
        if let Some(out) = try_override_ocr(&req).await {
            return out;
        }
        Ok(None)
    }
    async fn classify(&self, _req: VisionClassifyRequest) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}
