// =============================================================================
// Plik: router.js
// Opis: SPA router. Rejestruje screens, monteje aktywny w #content,
//       aktualizuje sidebar active state.
// =============================================================================

const screens = new Map();
let currentId = null;
let currentScreen = null;

export const Router = {
  register(id, screen) {
    screens.set(id, screen);
  },

  async navigate(id, params = null) {
    const screen = screens.get(id);
    if (!screen) {
      console.warn(`[router] unknown view: ${id}`);
      return;
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
    currentScreen = screen;

    // Sidebar active — drill-down widoki (params != null) nie sa pozycjami
    // w sidebarze, wiec nie czyscimy podswietlenia gdy nawigujemy z parametrami.
    if (!params) {
      document.querySelectorAll('.sidebar .nav-item[data-view]').forEach((el) => {
        el.classList.toggle('active', el.dataset.view === id);
      });
    }

    const content = document.getElementById('main');
    if (!content) return;

    // Tryb 1: screen.show(params) — kontroluje render i lifecycle samodzielnie
    // (uzywany przez mesh-detail i profile-report). Nie wymaga render/mount.
    if (typeof screen.show === 'function') {
      try {
        await screen.show(params || {});
      } catch (e) {
        console.error(`[router] show ${id} failed`, e);
        content.innerHTML = `<div style="padding:32px;"><h3 style="color:var(--danger);">Błąd ładowania widoku</h3><pre style="color:var(--text-2);font-family:monospace;">${e.message}</pre></div>`;
      }
      return;
    }

    // Tryb 2: render() + mount() — standardowe ekrany sidebar.
    content.innerHTML = '<div style="padding:48px;text-align:center;color:var(--text-3);">Ładowanie…</div>';
    try {
      const html = await screen.render(params || {});
      content.innerHTML = html;
      if (screen.mount) await screen.mount(params || {});
    } catch (e) {
      console.error(`[router] render ${id} failed`, e);
      content.innerHTML = `<div style="padding:32px;"><h3 style="color:var(--danger);">Błąd ładowania widoku</h3><pre style="color:var(--text-2);font-family:monospace;">${e.message}</pre></div>`;
    }
  },

  current() {
    return currentId;
  },

  init(defaultId) {
    if (defaultId) this.navigate(defaultId);
  },
};
