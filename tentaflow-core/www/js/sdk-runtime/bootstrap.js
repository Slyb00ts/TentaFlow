// =============================================================================
// File: sdk-runtime/bootstrap.js
// Description: Central bootstrap for SDK runtime (Phase 6 Step 3). Registers
// renderers for all component groups in the global `ComponentRenderer`
// registry. Called ONCE before the first panel render (chunk 3.7 cutover
// wires this into `addon-app.js` / the new shell).
//
// Sub-chunks 3.3a..3.3f register here sequentially; each group is idempotent
// (skip if already registered), so bootstrap can be called multiple times
// without side effects.
// =============================================================================

import { registerLayoutAtomicRenderers } from './layout-atomic-renderers.js';
import { registerLayoutContainersRenderers } from './layout-containers-renderers.js';
import { registerLayoutCardsRenderers } from './layout-cards-renderers.js';
import { registerLayoutNavRenderers } from './layout-nav-renderers.js';
import { registerLayoutBreadcrumbPaginationRenderers } from './layout-nav-breadcrumb-pagination.js';
import { registerLayoutSidebarTabsRenderers } from './layout-sidebar-tabs-renderer.js';
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
import { registerFormTagMentionRenderers } from './form-tag-mention-renderer.js';
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
import { registerDataSpecialisedRenderers } from './data-specialised-renderer.js';
import { registerDataMarkdownRenderer } from './data-markdown-renderer.js';
import { registerFeedbackInlineRenderers } from './feedback-inline-renderer.js';
import { registerFeedbackLoadingRenderers } from './feedback-loading-renderer.js';
import { registerFeedbackOverlayRenderers } from './feedback-overlay-renderer.js';
import { registerMoleculePageRenderers } from './molecule-page-renderer.js';
import { registerMoleculeShellRenderers } from './molecule-shell-renderer.js';
import { registerSpecializedMediaRenderers } from './specialized-media-renderer.js';
import { registerSpecializedAudioCaptureRenderer } from './specialized-audio-capture-renderer.js';
import { registerSpecializedContentRenderers } from './specialized-content-renderer.js';
export { SlotManager } from './slot-manager.js';

/// Registers all current component renderers. Called by the panel shell
/// at bootstrap. Idempotent — subsequent calls skip already-registered tags.
export function bootstrapSdkRuntime() {
  registerLayoutAtomicRenderers();
  registerLayoutContainersRenderers();
  registerLayoutCardsRenderers();
  registerLayoutNavRenderers();
  registerLayoutBreadcrumbPaginationRenderers();
  registerLayoutSidebarTabsRenderers();
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
  registerFormTagMentionRenderers();
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
  registerDataSpecialisedRenderers();
  registerDataMarkdownRenderer();
  registerFeedbackInlineRenderers();
  registerFeedbackLoadingRenderers();
  registerFeedbackOverlayRenderers();
  registerMoleculeShellRenderers();
  registerMoleculePageRenderers();
  registerSpecializedMediaRenderers();
  registerSpecializedAudioCaptureRenderer();
  registerSpecializedContentRenderers();
  // 3.3b Action: KOMPLETNE (13/13). 3.3c Form: KOMPLETNE (26/26).
  // 3.3d Data Display: KOMPLETNE (38/38). 3.3e Feedback: KOMPLETNE (14/14).
  // 3.3f Molecules: KOMPLETNE (12/12). 3.3g Specialized: KOMPLETNE (14/14).
  // 3.3a-5b Sidebar (0x010A) + Tabs (0x010B): registered (slot-driven).
  // Form TagInput (0x0308) + MentionInput (0x0309): registered.
}
