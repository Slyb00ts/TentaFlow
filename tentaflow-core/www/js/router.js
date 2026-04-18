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
    document.querySelectorAll('.nav-item[data-view]').forEach((el) => {
      el.classList.toggle('active', el.dataset.view === id);
    });

    // Topbar title.
    const titleEl = document.getElementById('topbar-title');
    if (titleEl) titleEl.textContent = screen.title ?? id;

    // Render.
    const content = document.getElementById('content');
    if (!content) return;
    content.innerHTML = '<div class="view-loader"><div class="view-loader-spinner"></div>Ładowanie…</div>';
    try {
      const html = await screen.render();
      content.innerHTML = html;
      if (screen.mount) await screen.mount();
    } catch (e) {
      console.error(`[router] render ${id} failed`, e);
      content.innerHTML = `<div class="card"><h3>Błąd ładowania widoku</h3><pre>${e.message}</pre></div>`;
    }
  },

  current() {
    return currentId;
  },

  init(defaultId) {
    document.querySelectorAll('.nav-item[data-view]').forEach((el) => {
      el.addEventListener('click', (e) => {
        e.preventDefault();
        this.navigate(el.dataset.view);
      });
    });
    if (defaultId) this.navigate(defaultId);
  },
};
