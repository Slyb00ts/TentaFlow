// =============================================================================
// Plik: modules/catalog/ManifestStore.js
// Opis: Centralny store dla manifestu silnikow AI. Ladowany asynchronicznie z
//       generated/services-manifest.js (ESM). Udostepnia API filtrowania per
//       kategoria i kompatybilnosc z platforma hosta (linux/macos/windows).
// Przyklad: await ManifestStore.init(); const list = ManifestStore.byCategory('llm');
// =============================================================================

const ManifestStore = (() => {
  'use strict';

  // Kanoniczna kolejnosc kategorii (uzywana do sortowania w GUI).
  const CATEGORY_ORDER = [
    'llm', 'stt', 'tts', 'embeddings', 'reranker', 'vision',
    'image-gen', 'video-gen', 'music-gen', 'model-3d-gen',
    'agents', 'tools'
  ];

  let services = [];
  let schemaVersion = null;
  let generatedAt = null;
  let loaded = false;
  let loadPromise = null;

  // Asynchroniczne zaladowanie manifestu z modulu ESM. Idempotentne.
  function init() {
    if (loaded) return Promise.resolve(true);
    if (loadPromise) return loadPromise;

    loadPromise = import('../../generated/services-manifest.js')
      .then(mod => {
        services = Array.isArray(mod.SERVICES) ? mod.SERVICES : [];
        schemaVersion = mod.SCHEMA_VERSION || null;
        generatedAt = mod.GENERATED_AT || null;
        loaded = true;
        return true;
      })
      .catch(err => {
        console.error('[ManifestStore] Nie udalo sie zaladowac manifestu:', err);
        services = [];
        loaded = true;
        return false;
      });

    return loadPromise;
  }

  function isLoaded() { return loaded; }
  function all() { return services.slice(); }
  function getSchemaVersion() { return schemaVersion; }
  function getGeneratedAt() { return generatedAt; }

  function byId(engineId) {
    return services.find(s => s && s.engine && s.engine.id === engineId) || null;
  }

  function byCategory(category) {
    return services.filter(s => s && s.engine && s.engine.category === category);
  }

  // Adapter zapewniajacy kompatybilnosc starego (V1: variant[]) i nowego
  // (V2: deploy.docker/native/external) schematu manifestu. Zwraca obiekt
  // { docker, native, external } z polami { platforms: [...] } (lub null).
  function deploySections(service) {
    if (!service) return { docker: null, native: null, external: null };

    // V2 — bezposrednio z service.deploy.
    if (service.deploy && typeof service.deploy === 'object') {
      const d = service.deploy;
      return {
        docker: normalizeSection(d.docker),
        native: normalizeSection(d.native),
        external: normalizeSection(d.external)
      };
    }

    // V1 — wyderywuj z variant[] po polu deploy_mode i target_os.
    const variants = Array.isArray(service.variant) ? service.variant : [];
    const collect = (mode) => {
      const platforms = new Set();
      variants.forEach(v => {
        if (!v) return;
        if (v.deploy_mode === mode || (mode === 'native' && v.deploy_mode === 'embedded')) {
          asArray(v.target_os).forEach(os => platforms.add(String(os).toLowerCase()));
        }
      });
      if (platforms.size === 0) return null;
      return { platforms: Array.from(platforms) };
    };

    return {
      docker: collect('docker'),
      native: collect('native'),
      external: collect('external')
    };
  }

  function normalizeSection(section) {
    if (!section || typeof section !== 'object') return null;
    const platforms = Array.isArray(section.platforms)
      ? section.platforms.map(p => String(p).toLowerCase())
      : [];
    return Object.assign({}, section, { platforms });
  }

  function asArray(value) {
    if (Array.isArray(value)) return value;
    if (value === null || value === undefined) return [];
    return [value];
  }

  // Zwraca true jesli silnik ma przynajmniej jedna sekcje deploy z platforma hosta.
  function isEngineCompatible(service, hostOs) {
    if (!service || !hostOs) return false;
    const os = String(hostOs).toLowerCase();
    const sec = deploySections(service);
    return [sec.docker, sec.native, sec.external].some(s => s && s.platforms.includes(os));
  }

  // Tryby deploymentu dostepne dla danego silnika i platformy hosta.
  // Zwraca podzbior ['docker', 'native', 'external'].
  function availableDeployMethods(service, hostOs) {
    const sec = deploySections(service);
    const os = String(hostOs || '').toLowerCase();
    const out = [];
    if (sec.docker && sec.docker.platforms.includes(os)) out.push('docker');
    if (sec.native && sec.native.platforms.includes(os)) out.push('native');
    if (sec.external && sec.external.platforms.includes(os)) out.push('external');
    return out;
  }

  function compatibleForHost(hostOs) {
    return services.filter(s => isEngineCompatible(s, hostOs));
  }

  // Zbior kategorii ktore maja przynajmniej jeden silnik (auto-hide pustych).
  function nonEmptyCategories() {
    const present = new Set();
    services.forEach(s => {
      if (s && s.engine && s.engine.category) present.add(s.engine.category);
    });
    return CATEGORY_ORDER.filter(c => present.has(c));
  }

  // Lista pozycji modeli — kompatybilna z V1 (model_preset) i V2 (model_presets).
  function modelPresets(service) {
    if (!service) return [];
    if (Array.isArray(service.model_presets)) return service.model_presets;
    if (Array.isArray(service.model_preset)) return service.model_preset;
    return [];
  }

  return {
    init,
    isLoaded,
    all,
    byId,
    byCategory,
    nonEmptyCategories,
    isEngineCompatible,
    compatibleForHost,
    availableDeployMethods,
    deploySections,
    modelPresets,
    getSchemaVersion,
    getGeneratedAt
  };
})();
