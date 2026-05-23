// =============================================================================
// Plik: sdk-runtime/bootstrap.js
// Opis: Centralny bootstrap SDK runtime'u (Faza 6 Krok 3). Rejestruje
// rendererów wszystkich grup komponentów w globalnym registry
// `ComponentRenderer`. Wywoływany RAZ przed pierwszym renderem panelu
// (chunk 3.7 cutover wpina to do `addon-app.js` / nowego shell'a).
//
// Sub-chunki 3.3a..3.3g rejestrują się tutaj kolejno; każda grupa jest
// idempotentna (skip jeśli już zarejestrowane), więc bootstrap można
// wywołać wielokrotnie bez efektu ubocznego.
// =============================================================================

import { registerLayoutAtomicRenderers } from './layout-atomic-renderers.js';
import { registerLayoutContainersRenderers } from './layout-containers-renderers.js';
import { registerLayoutCardsRenderers } from './layout-cards-renderers.js';
import { registerLayoutNavRenderers } from './layout-nav-renderers.js';
import { registerLayoutBreadcrumbPaginationRenderers } from './layout-nav-breadcrumb-pagination.js';
import { registerActionButtonRenderer } from './action-button-renderer.js';
import { registerActionIconButtonRenderer } from './action-icon-button-renderer.js';
import { registerActionLinkFabRenderers } from './action-link-fab-renderer.js';
import { registerActionButtonGroupRenderer } from './action-button-group-renderer.js';
import { registerActionMenuRenderers } from './action-menu-renderer.js';
import { registerActionBarsRenderers } from './action-bars-renderer.js';
import { registerFormAtomicRenderers } from './form-atomic-renderer.js';
import { registerFormTextRenderers } from './form-text-renderer.js';
import { registerFormSelectRenderers } from './form-select-renderer.js';
import { registerFormMultiSelectRenderers } from './form-multiselect-renderer.js';
import { registerFormComboboxRenderers } from './form-combobox-renderer.js';
import { registerFormDatetimeRenderers } from './form-datetime-renderer.js';

/// Rejestruje wszystkie aktualne renderery komponentów. Wywoływany przez
/// shell panelu w bootstrap'ie. Idempotentne — kolejne wywołania pomijają
/// już-zarejestrowane tagi.
export function bootstrapSdkRuntime() {
  registerLayoutAtomicRenderers();
  registerLayoutContainersRenderers();
  registerLayoutCardsRenderers();
  registerLayoutNavRenderers();
  registerLayoutBreadcrumbPaginationRenderers();
  registerActionButtonRenderer();
  registerActionIconButtonRenderer();
  registerActionLinkFabRenderers();
  registerActionButtonGroupRenderer();
  registerActionMenuRenderers();
  registerActionBarsRenderers();
  registerFormAtomicRenderers();
  registerFormTextRenderers();
  registerFormSelectRenderers();
  registerFormMultiSelectRenderers();
  registerFormComboboxRenderers();
  registerFormDatetimeRenderers();
  // 3.3b Action: KOMPLETNE (13/13). 3.3c Form: in progress (13/29 — atomic + text + selectors + datetime).
  // 3.3a-5b (Sidebar + Tabs) wymaga slot manager z chunka 3.5.
}
