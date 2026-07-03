// =============================================================================
// Plik: sw.js
// Opis: Service worker cache'ujacy CALY front. Przy instalacji precacheuje
//       wszystkie zasoby z wygenerowanej listy (sw-version.js -> build.rs) do
//       cache nazwanego build-hashem. Zmiana jakiegokolwiek pliku frontu zmienia
//       hash => zmienia bajty importScripts => browser wykrywa update SW i
//       przecacheowuje swiezy komplet, a activate kasuje stary cache. To znosi
//       koniecznosc recznego hard-reloadu po aktualizacji backendu/addonu.
//       WS/WT do daemona i /api/ nigdy nie sa cache'owane.
// =============================================================================

// Wygenerowany przez build.rs: self.__ASSET_BUILD_HASH + self.__ASSET_MANIFEST.
// Zmiana hasha zmienia tresc tego importu => browser traktuje SW jako nowy.
try {
  importScripts('/js/generated/sw-version.js');
} catch (e) {
  // Brak wygenerowanego pliku (np. front bez zbudowanego manifestu) — SW
  // dziala wtedy jako pass-through, bez precache.
}

const BUILD_HASH = self.__ASSET_BUILD_HASH || 'dev';
const CACHE_VERSION = `tentaflow-${BUILD_HASH}`;
// Pelna lista zasobow + bootstrap ('/' i manifest ESM czytany przez klienta).
const PRECACHE = [
  '/',
  '/index.html',
  '/js/generated/asset-manifest.js',
  ...(Array.isArray(self.__ASSET_MANIFEST) ? self.__ASSET_MANIFEST : []),
];

self.addEventListener('install', (event) => {
  event.waitUntil((async () => {
    const cache = await caches.open(CACHE_VERSION);
    // allowFail — pojedynczy 404 nie wywala instalacji calego kompletu.
    await Promise.allSettled(PRECACHE.map(async (url) => {
      try {
        const resp = await fetch(url, { cache: 'reload' });
        if (resp.ok) await cache.put(url, resp);
      } catch { /* ignore */ }
    }));
    await self.skipWaiting();
  })());
});

self.addEventListener('activate', (event) => {
  event.waitUntil((async () => {
    const names = await caches.keys();
    // Skasuj kazdy cache poza aktualnym build-hashem.
    await Promise.all(
      names.filter((n) => n !== CACHE_VERSION).map((n) => caches.delete(n)),
    );
    await self.clients.claim();
  })());
});

// Cache nazwany build-hashem => zawartosc jest niezmienna w obrebie builda,
// wiec cache-first jest bezpieczne i daje natychmiastowy start. Nowy build =
// nowy cache z precache. API/WS/WT zawsze przez network.
self.addEventListener('fetch', (event) => {
  const req = event.request;
  if (req.method !== 'GET') return;
  const url = new URL(req.url);
  if (
    url.pathname.startsWith('/api/') ||
    url.pathname.startsWith('/ws/') ||
    url.pathname.startsWith('/wt/')
  ) {
    return;
  }
  event.respondWith((async () => {
    const cache = await caches.open(CACHE_VERSION);
    const cached = await cache.match(req);
    if (cached) return cached;
    try {
      const fresh = await fetch(req);
      // Dogrywaj do cache zasoby frontu pominiete przy precache (np. lazy panele).
      if (
        fresh.ok &&
        (url.pathname === '/' ||
          url.pathname.startsWith('/js/') ||
          url.pathname.startsWith('/css/') ||
          url.pathname.startsWith('/i18n/') ||
          url.pathname === '/manifest.webmanifest' ||
          url.pathname === '/index.html')
      ) {
        cache.put(req, fresh.clone());
      }
      return fresh;
    } catch {
      // Offline — SPA fallback na index.html dla routingu klienta.
      const shell = await cache.match('/index.html');
      if (shell) return shell;
      return new Response('Offline', { status: 503 });
    }
  })());
});
