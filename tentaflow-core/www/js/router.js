// =============================================================================
// Plik: router.js
// Opis: SPA router. Rejestruje screens, monteje aktywny w #content,
//       aktualizuje sidebar active state.
// =============================================================================

import { escapeHtml } from '/js/utils.js';

const screens = new Map();
let currentId = null;
let currentParams = null;
let currentScreen = null;

/// Stable key for "is this the same route with the same parameters".
/// Two instances of one native app share `id` and differ only by `instance`,
/// so an id comparison alone cannot tell them apart.
function paramsKey(params) {
  if (!params) return '';
  return Object.keys(params)
    .sort()
    .map((k) => `${k}=${params[k]}`)
    .join('&');
}

// Dismiss overlay elements that live outside #main (wizards/dialogs appended to
// <body>). They otherwise survive a view switch and cover the next screen.
function closeStrayOverlays() {
  const main = document.getElementById('main');
  // Include BOTH backdrop kinds. A tf-window opened via TfWindow.open() appends
  // a separate `.tf-window-backdrop` div to <body>; closing the window doesn't
  // always take the backdrop with it, and a leaked full-screen backdrop then
  // silently eats every click on the next view. Remove backdrops directly.
  const overlays = document.querySelectorAll(
    'tf-window, tf-modal, .tf-modal-backdrop, .tf-window-backdrop',
  );
  overlays.forEach((el) => {
    if (main && main.contains(el)) return; // in-view overlays are the screen's own
    try {
      if (typeof el.close === 'function') el.close(true);
      else el.remove();
    } catch (_) {
      el.remove();
    }
  });
}

export const Router = {
  register(id, screen) {
    screens.set(id, screen);
  },

  /// Mounts a screen. Answers whether the navigation HAPPENED: the screen being
  /// left may refuse it (see `canUnmount`), and a caller that moved something
  /// else — a tab strip, the address bar — has to put its own state back.
  async navigate(id, params = null) {
    const screen = screens.get(id);
    if (!screen) {
      console.warn(`[router] unknown view: ${id}`);
      return false;
    }

    // Zamknij osierocone overlaye (tf-window/tf-modal wizardy dopinane do
    // <body>, poza #main) — inaczej modal „przecieka" na kolejny widok i blokuje
    // klikanie. Robimy to per-nawigacja, centralnie, zamiast w każdym module.
    closeStrayOverlays();

    // A screen may hold work that exists nowhere else — the TentaQuant notebook
    // keeps its cells in the view object until a save lands — and `unmount` is
    // told, not asked. `canUnmount` is where a screen ASKS, before anything is
    // torn down; a false answer leaves the current view mounted and untouched.
    // Re-mounting the SAME screen with the SAME parameters is not leaving it,
    // and it is not the user's move either — the shell repaints itself that way
    // after a language change, having already emptied #main, so a refusal there
    // would strand the user on a blank page. Two instances of one native app do
    // differ, though: switching laboratories leaves the notebook behind.
    const leaving =
      id !== currentId || paramsKey(params) !== paramsKey(currentParams);
    if (leaving && currentScreen && typeof currentScreen.canUnmount === 'function') {
      let allowed = true;
      try {
        allowed = await currentScreen.canUnmount(id);
      } catch (e) {
        console.error(`[router] canUnmount ${currentId} failed`, e);
      }
      if (!allowed) return false;
    }

    // Odpiecie poprzedniego — wspieramy oba style (`unmount` lub `cleanup`),
    // bo niektore widoki drill-down (np. mesh-detail) trzymaja interval'y i
    // sluchacze niezalezne od render/mount.
    if (currentScreen) {
      try {
        if (typeof currentScreen.unmount === 'function') await currentScreen.unmount();
        else if (typeof currentScreen.cleanup === 'function') await currentScreen.cleanup();
      } catch (e) {
        console.error(`[router] cleanup ${currentId} failed`, e);
      }
    }

    currentId = id;
    currentParams = params;
    currentScreen = screen;

    // Put the route in the URL so a screen can be bookmarked, shared and
    // survive a reload. Without this the hash was decoration: `init()` always
    // opened the dashboard and nothing listened for a change, so refreshing
    // mid-task threw the user back to the start. `replaceState` keeps the back
    // button meaningful — one entry per navigation, not per repaint.
    this.replaceParams(params);

    // Sidebar active — drill-down widoki (params != null) nie sa pozycjami
    // w sidebarze, wiec nie czyscimy podswietlenia gdy nawigujemy z parametrami.
    if (!params) {
      document.querySelectorAll('.sidebar .nav-item[data-view]').forEach((el) => {
        el.classList.toggle('active', el.dataset.view === id);
      });
    } else if (params.instance) {
      // A native app instance IS a sidebar item (one per installed instance),
      // unlike the drill-down views the branch above skips. Highlight exactly
      // the instance being opened, so two instances of one package never both
      // look active.
      document.querySelectorAll('.sidebar .nav-item[data-view]').forEach((el) => {
        el.classList.toggle(
          'active',
          el.dataset.view === id && el.dataset.instance === params.instance,
        );
      });
    }

    const content = document.getElementById('main');
    if (!content) return true;

    // Tryb 1: screen.show(params) — kontroluje render i lifecycle samodzielnie
    // (uzywany przez mesh-detail i profile-report). Nie wymaga render/mount.
    if (typeof screen.show === 'function') {
      try {
        await screen.show(params || {});
      } catch (e) {
        console.error(`[router] show ${id} failed`, e);
        content.innerHTML = `<div style="padding:32px;"><h3 style="color:var(--danger);">Błąd ładowania widoku</h3><pre style="color:var(--text-2);font-family:monospace;">${escapeHtml(e.message)}</pre></div>`;
      }
      return true;
    }

    // Tryb 2: render() + mount() — standardowe ekrany sidebar.
    content.innerHTML = '<div style="padding:48px;text-align:center;color:var(--text-3);">Ładowanie…</div>';
    try {
      const html = await screen.render(params || {});
      content.innerHTML = html;
      if (screen.mount) await screen.mount(params || {});
    } catch (e) {
      console.error(`[router] render ${id} failed`, e);
      content.innerHTML = `<div style="padding:32px;"><h3 style="color:var(--danger);">Błąd ładowania widoku</h3><pre style="color:var(--text-2);font-family:monospace;">${escapeHtml(e.message)}</pre></div>`;
    }
    return true;
  },

  replaceParams(params) {
    currentParams = params;
    try {
      const query = new URLSearchParams(
        Object.entries(params || {}).filter(([, value]) => value != null && value !== ''),
      ).toString();
      const next = `#/${currentId}${query ? `?${query}` : ''}`;
      if (window.location.hash !== next) window.history.replaceState(null, '', next);
    } catch { /* a URL we cannot write is not worth failing navigation over */ }
  },

  current() {
    return currentId;
  },

  /// Parameters the current screen was navigated with (`null` when none).
  /// A repaint that re-navigates (language switch) must pass these back, or a
  /// native app instance loses the `instance` id it is addressed by.
  currentParams() {
    return currentParams;
  },

  /// Reads `#/screen?a=b` into `{id, params}`; `null` when the hash names nothing.
  fromHash() {
    const raw = String(window.location.hash || '').replace(/^#\/?/, '');
    if (!raw) return null;
    const [id, query] = raw.split('?');
    if (!id || !screens.has(id)) return null;
    const params = query ? Object.fromEntries(new URLSearchParams(query)) : null;
    return { id, params };
  },

  init(defaultId) {
    // A hash in the address bar wins over the default: that is what makes a
    // pasted link open the screen it names.
    const target = this.fromHash();
    if (target) this.navigate(target.id, target.params);
    else if (defaultId) this.navigate(defaultId);

    // Back/forward and hand-edited URLs. `replaceState` above does not fire
    // hashchange, so this only ever reacts to the user moving.
    window.addEventListener('hashchange', async (ev) => {
      const next = this.fromHash();
      // Compare parameters too: two instances of one native app share `id` and
      // differ only by `?instance=`, so an id-only check would leave the screen
      // showing instance A while the address bar says B.
      if (!next) return;
      if (next.id === currentId && paramsKey(next.params) === paramsKey(currentParams)) return;
      const moved = await this.navigate(next.id, next.params);
      // A refused navigation leaves the OLD screen mounted, so the address bar
      // has to name it again — otherwise the URL promises a view nobody is on.
      // `oldURL` is the only reliable source: a screen may have rewritten the
      // hash itself (deep links inside one view) since the router last wrote it.
      if (!moved && ev.oldURL) window.history.replaceState(null, '', ev.oldURL);
    });
  },
};
