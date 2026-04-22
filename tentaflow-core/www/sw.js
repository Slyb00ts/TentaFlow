// =============================================================================
// Plik: sw.js
// Opis: Minimalny service worker dla PWA — cache shell (index.html, JS, CSS,
//       i18n) zeby aplikacja uruchamiala sie offline po pierwszej wizycie.
//       WS do daemona oczywiscie nie zadziala offline, ale login screen +
//       overlay "Utracono polaczenie" wyswietla sie natychmiast zamiast
//       biaglego ekranu.
// =============================================================================

const CACHE_VERSION = 'tentaflow-v1';
const SHELL_ASSETS = [
  '/',
  '/index.html',
  '/manifest.webmanifest',
  '/css/style.css',
  '/css/controls.css',
  '/css/compat.css',
  '/css/addons.css',
  '/css/notes.css',
  '/css/connection-overlay.css',
  '/js/app.js',
  '/js/protocol/codec.js',
  '/js/protocol/transport.js',
  '/js/protocol/binary-ws-client.js',
  '/js/protocol/api-binary-shim.js',
  '/js/i18n.js',
  '/js/router.js',
];

self.addEventListener('install', (event) => {
  event.waitUntil((async () => {
    const cache = await caches.open(CACHE_VERSION);
    // Use addAll with allowFail semantics — individual 404 (np. brak jeszcze
    // niektorych plikow) nie wywali instalacji.
    await Promise.allSettled(SHELL_ASSETS.map(async (url) => {
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
    await Promise.all(names.filter((n) => n !== CACHE_VERSION).map((n) => caches.delete(n)));
    await self.clients.claim();
  })());
});

// Network-first dla JS/CSS, offline fallback z cache. API requesty (/api/*,
// /ws/api) nigdy nie cachujemy — zawsze idz przez network.
self.addEventListener('fetch', (event) => {
  const req = event.request;
  if (req.method !== 'GET') return;
  const url = new URL(req.url);
  if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/ws/') || url.pathname.startsWith('/wt/')) {
    return;
  }
  event.respondWith((async () => {
    try {
      const fresh = await fetch(req);
      if (fresh.ok && (url.pathname === '/' || url.pathname.startsWith('/js/') || url.pathname.startsWith('/css/') || url.pathname.startsWith('/i18n/') || url.pathname === '/manifest.webmanifest' || url.pathname === '/index.html')) {
        const cache = await caches.open(CACHE_VERSION);
        cache.put(req, fresh.clone());
      }
      return fresh;
    } catch {
      const cached = await caches.match(req);
      if (cached) return cached;
      // SPA fallback — dla /mesh, /chat itp. oddaj index.html.
      const shell = await caches.match('/index.html');
      if (shell) return shell;
      return new Response('Offline', { status: 503 });
    }
  })());
});
