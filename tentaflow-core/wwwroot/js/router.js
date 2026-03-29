// =============================================================================
// Plik: router.js
// Opis: SPA router - nawigacja miedzy widokami bez przeladowania strony.
//       Oparty na viewId (nie URL hash), aktualizuje sidebar i tytul.
// Przyklad: ViewRouter.navigate('services');
// =============================================================================

const ViewRouter = (() => {
  'use strict';

  let currentView = 'dashboard';
  let views = {};

  // Rejestracja widoku
  function register(viewId, module) {
    views[viewId] = module;
  }

  // Nawigacja do widoku
  function navigate(viewId) {
    if (!views[viewId]) {
      console.warn('Nieznany widok:', viewId);
      return;
    }

    // Odmontuj aktualny widok
    if (views[currentView] && typeof views[currentView].unmount === 'function') {
      views[currentView].unmount();
    }

    currentView = viewId;

    // Aktualizuj tytul strony
    const pageTitle = document.getElementById('page-title');
    if (pageTitle) {
      pageTitle.textContent = I18n.t(`nav.${viewId}`);
    }

    // Aktualizuj aktywny element sidebar
    document.querySelectorAll('.nav-item').forEach(item => {
      item.classList.toggle('active', item.dataset.view === viewId);
    });

    // Renderuj nowy widok
    const content = document.getElementById('content');
    if (content && views[viewId]) {
      content.innerHTML = views[viewId].render();
      if (typeof views[viewId].mount === 'function') {
        views[viewId].mount();
      }
    }
  }

  // Pobierz aktualny widok
  function getCurrentView() {
    return currentView;
  }

  // Inicjalizacja obslugi klikniec w sidebar
  function init() {
    document.querySelectorAll('.nav-item[data-view]').forEach(item => {
      item.addEventListener('click', (e) => {
        e.preventDefault();
        navigate(item.dataset.view);
      });
    });
  }

  return {
    register,
    navigate,
    getCurrentView,
    init,
  };
})();
