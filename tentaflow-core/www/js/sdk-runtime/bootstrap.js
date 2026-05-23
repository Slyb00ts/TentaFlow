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
import { registerFormRangeNumericRenderers } from './form-range-numeric-renderer.js';
import { registerFormFileColorRenderers } from './form-file-color-renderer.js';
import { registerFormRadioGroupsRenderers } from './form-radio-groups-renderer.js';
import { registerFormWrappersRenderers } from './form-wrappers-renderer.js';
import { registerDataTextRenderers } from './data-text-renderer.js';
import { registerDataStatLabelsRenderers } from './data-stat-labels-renderer.js';
import { registerDataAvatarListsRenderers } from './data-avatar-lists-renderer.js';
import { registerDataTreeEmptyRenderers } from './data-tree-empty-renderer.js';
import { registerDataListRenderer } from './data-list-renderer.js';
import { registerDataTableRenderer } from './data-table-renderer.js';
import { registerDataSparklineRenderer } from './data-sparkline-renderer.js';
import { registerDataLineChartRenderer } from './data-line-chart-renderer.js';
import { registerDataAreaChartRenderer } from './data-area-chart-renderer.js';
import { registerDataBarChartRenderers } from './data-bar-chart-renderer.js';
import { registerDataPieChartRenderer } from './data-pie-chart-renderer.js';
import { registerDataHeatmapGaugeRenderers } from './data-heatmap-gauge-renderer.js';
import { registerDataProgressRatingRenderers } from './data-progress-rating-renderer.js';
import { registerDataDiffDlJsonRenderers } from './data-diff-dl-json-renderer.js';

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
  registerFormRangeNumericRenderers();
  registerFormFileColorRenderers();
  registerFormRadioGroupsRenderers();
  registerFormWrappersRenderers();
  registerDataTextRenderers();
  registerDataStatLabelsRenderers();
  registerDataAvatarListsRenderers();
  registerDataTreeEmptyRenderers();
  registerDataListRenderer();
  registerDataTableRenderer();
  registerDataSparklineRenderer();
  registerDataLineChartRenderer();
  registerDataAreaChartRenderer();
  registerDataBarChartRenderers();
  registerDataPieChartRenderer();
  registerDataHeatmapGaugeRenderers();
  registerDataProgressRatingRenderers();
  registerDataDiffDlJsonRenderers();
  // 3.3b Action: KOMPLETNE (13/13). 3.3c Form: KOMPLETNE (26/26).
  // 3.3d Data Display: in progress (33/38 — + Diff (0x021F) + DataDefinitionList (0x0221) + JsonViewer (0x0222)). EmptyState (§2 0x0003) tymczasowo w data-list-renderer.
  // 3.3a-5b (Sidebar + Tabs) wymaga slot manager z chunka 3.5.
}
