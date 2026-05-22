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

/// Rejestruje wszystkie aktualne renderery komponentów. Wywoływany przez
/// shell panelu w bootstrap'ie. Idempotentne — kolejne wywołania pomijają
/// już-zarejestrowane tagi.
export function bootstrapSdkRuntime() {
  registerLayoutAtomicRenderers();
  registerLayoutContainersRenderers();
  registerLayoutCardsRenderers();
  // Kolejne grupy dołączane w nastepnych chunkach 3.3a-4, 3.3b, 3.3c, ...
}
