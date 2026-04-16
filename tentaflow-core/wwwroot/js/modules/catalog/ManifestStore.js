// =============================================================================
// Plik: modules/catalog/ManifestStore.js
// Opis: Centralny store dla manifestu silnikow AI. Ladowany asynchronicznie z
//       generated/services-manifest.js (ESM). Udostepnia API filtrowania per
//       kategoria i kompatybilnosc z wezlami (os/arch/gpu).
// Przyklad: await ManifestStore.init(); const list = ManifestStore.byCategory('llm');
// =============================================================================

const ManifestStore = (() => {
  'use strict';

  // Kanoniczna lista 12 kategorii z planu (kolejnosc = kolejnosc renderu)
  const KNOWN_CATEGORIES = [
    'llm', 'stt', 'tts', 'embeddings', 'reranker', 'vision',
    'image-gen', 'video-gen', 'music-gen', 'model-3d-gen',
    'agents', 'tools'
  ];

  let engines = [];
  let schemaVersion = null;
  let generatedAt = null;
  let loaded = false;
  let loadPromise = null;

  // Asynchroniczne zaladowanie manifestu z modulu ESM.
  // Idempotentne — kolejne wywolania zwracaja te same dane.
  function init() {
    if (loaded) return Promise.resolve(true);
    if (loadPromise) return loadPromise;

    loadPromise = import('../../generated/services-manifest.js')
      .then(mod => {
        engines = Array.isArray(mod.SERVICES) ? mod.SERVICES : [];
        schemaVersion = mod.SCHEMA_VERSION || null;
        generatedAt = mod.GENERATED_AT || null;
        loaded = true;
        return true;
      })
      .catch(err => {
        console.error('[ManifestStore] Nie udalo sie zaladowac manifestu:', err);
        engines = [];
        loaded = true;
        return false;
      });

    return loadPromise;
  }

  function isLoaded() {
    return loaded;
  }

  function allCategories() {
    return KNOWN_CATEGORIES.slice();
  }

  // Zwraca silniki ktorych engine.category === cat lub also_serves zawiera cat.
  function byCategory(cat) {
    return engines.filter(e => {
      const eng = e.engine || {};
      const also = Array.isArray(eng.also_serves) ? eng.also_serves : [];
      return eng.category === cat || also.indexOf(cat) !== -1;
    });
  }

  function byId(engineId) {
    return engines.find(e => e.engine && e.engine.id === engineId) || null;
  }

  function isEmpty(cat) {
    return byCategory(cat).length === 0;
  }

  // Normalizuje pole ktore w TOML moze byc stringiem lub tablica do tablicy.
  function asArray(value) {
    if (Array.isArray(value)) return value;
    if (value === null || value === undefined) return [];
    return [value];
  }

  // Filtruje warianty silnika po (os, arch, gpu) wezla docelowego.
  // Gdy nodeCapabilities = null/undefined zwraca wszystkie warianty.
  function compatibleVariants(engine, nodeCapabilities) {
    const variants = (engine && Array.isArray(engine.variant)) ? engine.variant : [];
    if (!nodeCapabilities) return variants.slice();

    const os = (nodeCapabilities.os || '').toLowerCase();
    const arch = (nodeCapabilities.arch || '').toLowerCase();
    const gpu = (nodeCapabilities.gpu || 'cpu').toLowerCase();

    return variants.filter(v => {
      const vOs = asArray(v.target_os).map(s => String(s).toLowerCase());
      const vArch = asArray(v.target_arch).map(s => String(s).toLowerCase());
      const vGpu = asArray(v.gpu_backend).map(s => String(s).toLowerCase());

      const osOk = vOs.length === 0 || vOs.indexOf('any') !== -1 || vOs.indexOf(os) !== -1;
      const archOk = vArch.length === 0 || vArch.indexOf('any') !== -1 || vArch.indexOf(arch) !== -1;
      const gpuOk = vGpu.length === 0 || vGpu.indexOf('any') !== -1 || vGpu.indexOf(gpu) !== -1;

      return osOk && archOk && gpuOk;
    });
  }

  function getSchemaVersion() {
    return schemaVersion;
  }

  function getGeneratedAt() {
    return generatedAt;
  }

  function getAllEngines() {
    return engines.slice();
  }

  return {
    init,
    isLoaded,
    allCategories,
    byCategory,
    byId,
    isEmpty,
    compatibleVariants,
    getSchemaVersion,
    getGeneratedAt,
    getAllEngines
  };
})();
