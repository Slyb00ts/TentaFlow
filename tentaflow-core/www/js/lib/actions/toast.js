// ===== File: lib/actions/toast.js — the "done, undo?" toast that ends every shared action =====

import { TfToast } from '/js/components/tf-toast.js';
import { t } from './fields.js';

const DEFAULT_TIMEOUT_MS = 8000;

const describe = (err) => String(err?.message || err || t('error_generic'));

/**
 * Shows `message` with an "Undo" button for `timeoutMs`. `onUndo` may return a
 * promise: the button is locked while it runs, and a refusal is reported in a
 * second toast instead of vanishing. Returns the toast element.
 */
export function showUndoToast({ message, onUndo, timeoutMs = DEFAULT_TIMEOUT_MS, undoLabel = t('undo') }) {
  let running = false;
  return TfToast.show({
    tone: 'success',
    message,
    duration: timeoutMs,
    action: {
      label: undoLabel,
      async onClick(toast) {
        if (running) return;
        running = true;
        toast.querySelector('tf-button')?.setAttribute('disabled', '');
        try {
          await onUndo();
        } catch (err) {
          TfToast.show({ tone: 'danger', message: t('undo_failed', { message: describe(err) }) });
        }
        toast.dismiss();
      },
    },
  });
}

/** Shows the toast an action's result asks for: undoable when it carries `undo`, plain when it only has a message. */
export function showResultToast(result) {
  if (!result || typeof result !== 'object') return;
  if (typeof result.undo === 'function') {
    showUndoToast({ message: result.message ?? t('done'), onUndo: result.undo });
  } else if (result.message) {
    TfToast.show({ tone: 'success', message: result.message });
  }
}
