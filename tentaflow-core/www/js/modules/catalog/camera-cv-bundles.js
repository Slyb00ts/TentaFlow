// =============================================================================
// File: modules/catalog/camera-cv-bundles.js
// Purpose: Shared list of shareable vision model-bundle refs for the /models
//   endpoints. Mirrors the backend source of truth (`BUNDLES` in
//   src/vision/camera_cv_models.rs + the `vision-all` pseudo-bundle) — keep in
//   sync when a new camera-CV engine bundle is added there.
// =============================================================================

/// Pseudo-bundle exposing every servable file in vision_models_dir().
export const BUNDLE_REF_ALL = 'vision-all';

/// Fixed camera-CV engine bundles (backend `is_camera_cv_engine`).
export const CAMERA_CV_BUNDLE_IDS = [
  'rfdetr-adr',
  'nalepka-stan',
  'plate-ocr',
  'depth-native',
];

/// True when the deploy wizard should offer the "Custom" bundle-source tab —
/// the engine's weights are pulled as a camera-CV bundle at deploy time.
export function isCameraCvEngineId(engineId) {
  return CAMERA_CV_BUNDLE_IDS.includes(String(engineId || '').toLowerCase());
}

/// Scope resources offered for `model_bundle` API-key grants: `vision-all`
/// first (covers every bundle), then the per-engine bundles.
export function modelBundleScopeResources(t) {
  const allLabel = typeof t === 'function'
    ? t('access_keys.bundle_all', 'Wszystkie modele wizyjne (vision-all)')
    : 'vision-all';
  return [
    { id: BUNDLE_REF_ALL, name: allLabel },
    ...CAMERA_CV_BUNDLE_IDS.map((id) => ({ id, name: id })),
  ];
}
