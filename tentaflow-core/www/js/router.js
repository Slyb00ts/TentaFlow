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

  async navigate(id) {
    const screen = screens.get(id);
    if (!screen) {
      console.warn(`[router] unknown view: ${id}`);
      return;
    }

    // Unmount poprzedniego.
    if (currentScreen?.unmount) {
      try {
        await currentScreen.unmount();
      } catch (e) {
        console.error(`[router] unmount ${currentId} failed`, e);
      }
    }

    currentId = id;
    currentScreen = screen;

    // Sidebar active.
    document.querySelectorAll('.sidebar .nav-item[data-view]').forEach((el) => {
      el.classList.toggle('active', el.dataset.view === id);
    });

    // Render do #main (wymiana calej zawartosci main, screen sam buduje page-header).
    const content = document.getElementById('main');
    if (!content) return;
    content.innerHTML = '<div style="padding:48px;text-align:center;color:var(--text-3);">Ładowanie…</div>';
    try {
      const html = await screen.render();
      content.innerHTML = html;
      if (screen.mount) await screen.mount();
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
