// =============================================================================
// Plik: modules/catalog/manifest-store.js
// Opis: Store manifestu silnikow AI — ladowany asynchronicznie z
//       /js/generated/services-manifest.js (ESM, produkt build.rs).
//       Udostepnia filtrowanie per kategoria oraz kompatybilnosc z host OS.
// =============================================================================

const CATEGORY_ORDER = [
  'llm', 'stt', 'tts', 'embeddings', 'reranker', 'vision',
  'image-gen', 'video-gen', 'music-gen', 'model-3d-gen',
  'agents', 'tools',
];

let services = [];
let schemaVersion = null;
let generatedAt = null;
let loaded = false;
let loadPromise = null;

/// Idempotentny loader manifestu. Zwraca true przy sukcesie, false przy bledzie
/// (fallback na pusty manifest).
export async function init() {
  if (loaded) return true;
  if (loadPromise) return loadPromise;

  loadPromise = import('/js/generated/services-manifest.js')
    .then((mod) => {
      services = Array.isArray(mod.SERVICES) ? mod.SERVICES : [];
      schemaVersion = mod.SCHEMA_VERSION || null;
      generatedAt = mod.GENERATED_AT || null;
      loaded = true;
      return true;
    })
    .catch((err) => {
      console.error('[manifest-store] load failed:', err);
      services = [];
      loaded = true;
      return false;
    });

  return loadPromise;
}

export function isLoaded() {
  return loaded;
}

export function all() {
  return services.slice();
}

export function getSchemaVersion() {
  return schemaVersion;
}

export function getGeneratedAt() {
  return generatedAt;
}

export function byId(engineId) {
  return services.find((s) => s?.engine?.id === engineId) || null;
}

export function byCategory(category) {
  return services.filter((s) => s?.engine?.category === category);
}

function normalizeSection(section) {
  if (!section || typeof section !== 'object') return null;
  const platforms = Array.isArray(section.platforms)
    ? section.platforms.map((p) => String(p).toLowerCase())
    : [];
  return Object.assign({}, section, { platforms });
}

/// Zwraca `{ docker, native, external }` z `platforms: string[]` dla each sekcji.
export function deploySections(service) {
  if (!service?.deploy) return { docker: null, native: null, external: null };
  const d = service.deploy;
  return {
    docker: normalizeSection(d.docker),
    native: normalizeSection(d.native),
    external: normalizeSection(d.external),
  };
}

export function isEngineCompatible(service, hostOs) {
  if (!service || !hostOs) return false;
  const os = String(hostOs).toLowerCase();
  const sec = deploySections(service);
  return [sec.docker, sec.native, sec.external].some((s) => s && s.platforms.includes(os));
}

/// Podzbior `['docker', 'native', 'external']` dostepnych dla hostu.
export function availableDeployMethods(service, hostOs) {
  const sec = deploySections(service);
  const os = String(hostOs || '').toLowerCase();
  const out = [];
  if (sec.docker && sec.docker.platforms.includes(os)) out.push('docker');
  if (sec.native && sec.native.platforms.includes(os)) out.push('native');
  if (sec.external && sec.external.platforms.includes(os)) out.push('external');
  return out;
}

export function compatibleForHost(hostOs) {
  return services.filter((s) => isEngineCompatible(s, hostOs));
}

/// Lista kategorii z >=1 silnikiem — auto-hide pustych.
export function nonEmptyCategories() {
  const present = new Set();
  services.forEach((s) => {
    if (s?.engine?.category) present.add(s.engine.category);
  });
  return CATEGORY_ORDER.filter((c) => present.has(c));
}

/// Lista presetow modeli dla engine — obsluguje oba warianty schematu.
export function modelPresets(service) {
  if (!service) return [];
  if (Array.isArray(service.model_presets)) return service.model_presets;
  if (Array.isArray(service.model_preset)) return service.model_preset;
  return [];
}
