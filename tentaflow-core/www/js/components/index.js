// =============================================================================
// Plik: index.js
// Opis: Rejestracja wszystkich web-components biblioteki kontrolek TentaFlow.
//       Importowany raz z app.js; side-effect: define() w customElements.
//       Re-eksportuje helpery (TfWindow.open, confirm).
// Przyklad: import './components/index.js';
// =============================================================================

import './tf-button.js';
import './tf-input.js';
import './tf-pin-input.js';
import './tf-textarea.js';
import './tf-searchbox.js';
import './tf-select.js';
import './tf-toggle.js';
import './tf-tabs.js';
import './tf-chip.js';
import './tf-table.js';
import './tf-menu.js';
import './tf-window.js';
import './tf-segmented.js';
import './tf-screen.js';
import './tf-addon-ui-frame/tf-addon-ui-frame.js';

export { TfButton } from './tf-button.js';
export { TfInput } from './tf-input.js';
export { TfPinInput } from './tf-pin-input.js';
export { TfTextarea } from './tf-textarea.js';
export { TfSearchbox } from './tf-searchbox.js';
export { TfSelect } from './tf-select.js';
export { TfToggle } from './tf-toggle.js';
export { TfTabs, TfTab } from './tf-tabs.js';
export { TfChip } from './tf-chip.js';
export { TfTable, TfColumn } from './tf-table.js';
export { TfMenu, TfMenuItem, TfMenuDivider } from './tf-menu.js';
export { TfWindow, openTfWindow, tfConfirm } from './tf-window.js';
export { TfSegmented } from './tf-segmented.js';
export { TfScreen } from './tf-screen.js';
export { TfAddonUiFrame } from './tf-addon-ui-frame/tf-addon-ui-frame.js';
