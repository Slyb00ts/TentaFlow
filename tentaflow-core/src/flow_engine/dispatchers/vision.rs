// =============================================================================
// Plik: flow_engine/dispatchers/vision.rs
// Opis: VisionDispatcher — camera-CV primitives (OCR, state classify) for flow
//       nodes. Mirrors the LLM/STT/TTS dispatcher pattern: a narrow trait the
//       node adapters call, backed by the centralized Burn runners. The model is
//       chosen via an alias (resolved + permission-gated by the impl), so flows
//       stay decoupled from concrete model files.
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

use crate::flow_engine::dispatcher::CallProvenance;

/// OCR request: a tightly-packed RGB24 crop + its dimensions, the model alias,
/// and the calling addon (for alias visibility / permission gating).
#[derive(Clone)]
pub struct VisionOcrRequest {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub alias: String,
    pub caller_addon_id: Option<String>,
    /// §2.5 — server-minted provenance of the flow this call belongs to. No
    /// `Default`: a defaulted stamp is the silent `system` value the design
    /// forbids, so both request types lost their `Default` derive with it.
    pub provenance: CallProvenance,
}

/// State-classification request (same shape as OCR; different model alias).
#[derive(Clone)]
pub struct VisionClassifyRequest {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub alias: String,
    pub caller_addon_id: Option<String>,
    /// §2.5 — server-minted provenance of the flow this call belongs to. No
    /// `Default`: a defaulted stamp is the silent `system` value the design
    /// forbids, so both request types lost their `Default` derive with it.
    pub provenance: CallProvenance,
}

#[async_trait]
pub trait VisionDispatcher: Send + Sync {
    /// Reads a plate/code string from an RGB crop. `None` = nothing readable.
    async fn ocr(&self, req: VisionOcrRequest) -> Result<Option<String>>;
    /// Classifies placard/label condition tags from an RGB crop (multi-label).
    async fn classify(&self, req: VisionClassifyRequest) -> Result<Vec<String>>;
}
