// ===== File: lib/actions/bulk.js — the bar that appears over a tf-table while rows are ticked =====

import { TfToast } from '/js/components/tf-toast.js';
import '/js/components/tf-button.js';
import { t } from './fields.js';

const selectedRows = (table) => table.rows.filter((row) => row && row._selected);

/**
 * Shows a toolbar above `table` (a `tf-table selectable="multi"`) whenever rows
 * are ticked: the count, one button per action and "Clear selection".
 * `actions` = [{ id, label, icon, danger, disabled, run(rows, { subject, anchor }) }];
 * `run` may return a promise — the bar is locked meanwhile and a rejection is
 * reported in a toast. `subject` is the ready text for the action windows
 * ("3 selected items"), `anchor` the pressed button (focus returns to it).
 *
 * The bar follows `row-select` and `select-all`; call `update()` after the
 * caller replaced `table.rows` (a reload drops the ticks, so the bar must hide).
 * Returns `{ bar, update, clear, selected, destroy }`.
 */
export function attachBulkBar(table, actions) {
  const bar = document.createElement('div');
  bar.className = 'tf-bulkbar';
  bar.setAttribute('role', 'toolbar');
  bar.setAttribute('aria-label', t('bulk.toolbar'));
  bar.hidden = true;
  const count = document.createElement('span');
  count.className = 'tf-bulkbar__count';
  count.setAttribute('aria-live', 'polite');
  bar.appendChild(count);
  const buttons = actions.map((action) => {
    const btn = document.createElement('tf-button');
    btn.setAttribute('variant', action.danger ? 'danger-outline' : 'secondary');
    btn.setAttribute('size', 'sm');
    if (action.icon) btn.setAttribute('icon', action.icon);
    btn.setAttribute('label', action.label);
    btn.dataset.action = action.id;
    if (action.disabled) btn.setAttribute('disabled', '');
    bar.appendChild(btn);
    return { btn, action };
  });
  const clearBtn = document.createElement('tf-button');
  clearBtn.setAttribute('variant', 'ghost');
  clearBtn.setAttribute('size', 'sm');
  clearBtn.setAttribute('label', t('bulk.clear'));
  clearBtn.dataset.clear = '';
  bar.appendChild(clearBtn);
  table.before(bar);

  let busy = false;
  const update = () => {
    const n = selectedRows(table).length;
    bar.hidden = n === 0;
    count.textContent = t('bulk.selected', { count: n });
  };
  const clear = () => {
    for (const row of selectedRows(table)) row._selected = false;
    table.rows = table.rows;
    update();
  };
  const lock = (on) => {
    busy = on;
    for (const { btn, action } of buttons) btn.toggleAttribute('disabled', on || Boolean(action.disabled));
    clearBtn.toggleAttribute('disabled', on);
  };

  bar.addEventListener('click', async (e) => {
    if (busy) return;
    const btn = e.target.closest('tf-button');
    if (!btn || btn.hasAttribute('disabled')) return;
    if ('clear' in btn.dataset) { clear(); return; }
    const entry = buttons.find((b) => b.btn === btn);
    if (!entry) return;
    const rows = selectedRows(table);
    lock(true);
    try {
      await entry.action.run(rows, { subject: t('bulk.subject', { count: rows.length }), anchor: btn });
    } catch (err) {
      TfToast.show({ tone: 'danger', message: String(err?.message || err || t('error_generic')) });
    } finally {
      lock(false);
      update();
    }
  });
  table.addEventListener('row-select', update);
  table.addEventListener('select-all', update);
  update();

  return {
    bar,
    update,
    clear,
    selected: () => selectedRows(table),
    destroy() {
      table.removeEventListener('row-select', update);
      table.removeEventListener('select-all', update);
      bar.remove();
    },
  };
}
