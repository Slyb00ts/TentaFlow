// ===== File: process-import.js — the window that reports what a Camunda import found before anything is loaded =====

import { escapeAttr, escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-window.js';
import '/js/components/tf-table.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import { processImportCounts, processImportGroups, processImportLoadable, processImportSummaryText, processImportTranslations } from './bpmn.js';

const SEVERITY_CHIP = { fatal: 'err', warning: 'warn', info: 'info' };

const columns = (specs) => specs.map(([key, label, attrs = '']) => `<tf-column key="${key}" label="${escapeAttr(I18n.t(label))}" ${attrs}></tf-column>`).join('');

const twoLines = (title, sub) => `<div class="tf-table__cell-title">${escapeHtml(title)}</div>${sub ? `<div class="tf-table__cell-sub">${escapeHtml(sub)}</div>` : ''}`;

/**
 * Opens the result of an import. A clean import never gets here. With a fatal
 * diagnostic nothing is loadable: the window only explains and closes. Otherwise
 * "Import as draft" is the author's consent to the translated expressions;
 * `onImport()` loads the model and answers whether it did, and from then on a
 * line with an element shows it on the canvas through `onReveal(row)`.
 */
export function openProcessImportReview({ response, onImport, onReveal }) {
  const loadable = processImportLoadable(response);
  const counts = processImportCounts(response);
  let loaded = false;
  let busy = false;
  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('bpmn.import_review_title'));
  win.setAttribute('icon', 'arrow-up');
  win.setAttribute('modal', '');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '1000');
  win.setAttribute('min-width', '360');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.classList.add('fb-process-window');
  win.innerHTML = '<div slot="body" class="fb-import-review" data-review></div><div slot="footer" data-footer></div>';
  const body = win.querySelector('[data-review]');
  const footer = win.querySelector('[data-footer]');

  const revealButton = (row) => {
    if (!row.nodeId) return null;
    const button = document.createElement('tf-button');
    button.setAttribute('variant', 'secondary');
    button.setAttribute('size', 'sm');
    button.textContent = I18n.t('bpmn.import_show_element');
    button.addEventListener('click', () => reveal(row));
    return button;
  };
  const reveal = (row) => {
    if (!loaded || !row.nodeId) return;
    onReveal(row);
    win.close(true);
  };
  const table = (rows, specs, emptyKey) => {
    const element = document.createElement('tf-table');
    element.setAttribute('empty-message', I18n.t(emptyKey));
    element.innerHTML = columns(specs);
    element.rows = rows;
    element.addEventListener('row-click', (event) => reveal(event.detail.row));
    if (loaded) {
      element.rowActions = (row) => revealButton(row);
      element.rowActionsKey = (row) => `${row.nodeId ?? ''}`;
    }
    return element;
  };

  function render() {
    const model = loadable ? response.model : null;
    const alerts = document.createElement('div');
    const tone = !loadable ? 'danger' : counts.warning ? 'warning' : 'info';
    const consent = loaded ? ['success', 'bpmn.import_loaded'] : loadable ? ['warning', 'bpmn.import_consent'] : ['danger', 'bpmn.import_blocked'];
    alerts.innerHTML = `<tf-alert data-summary tone="${tone}" message="${escapeAttr(processImportSummaryText(counts))}"></tf-alert>
      <tf-alert data-consent tone="${consent[0]}" message="${escapeAttr(I18n.t(consent[1]))}"></tf-alert>`;
    const sections = [alerts];
    for (const group of processImportGroups(response.diagnostics, model)) {
      const section = document.createElement('section');
      section.dataset.severity = group.severity;
      section.innerHTML = `<h3><tf-chip status="${SEVERITY_CHIP[group.severity]}">${escapeHtml(I18n.t(`bpmn.import_severity_${group.severity}`))}</tf-chip> ${escapeHtml(I18n.t('bpmn.import_group_count', { count: group.rows.length }))}</h3>`;
      section.append(table(group.rows.map((row) => ({ ...row, message: twoLines(row.text, row.text === row.detail ? '' : row.detail) })),
        [['code', 'bpmn.import_column_code', 'width="26%"'], ['element', 'bpmn.import_column_element', 'width="24%"'],
          ['message', 'bpmn.import_column_message', 'renderer="html" fill']], 'bpmn.import_no_diagnostics'));
      sections.push(section);
    }
    const translations = processImportTranslations(response.translations, model);
    if (translations.length) {
      const section = document.createElement('section');
      section.dataset.translations = '';
      section.innerHTML = `<h3>${escapeHtml(I18n.t('bpmn.import_translations_title'))}</h3>
        <p class="fb-field-hint">${escapeHtml(I18n.t('bpmn.import_translations_hint'))}</p>`;
      section.append(table(translations.map((row) => ({ ...row,
        warningChips: row.warnings.map((warning) => `<tf-chip status="warn" title="${escapeAttr(warning.text)}">${escapeHtml(warning.code)}</tf-chip>`).join(' ') })),
      [['original', 'bpmn.import_column_before', 'width="26%"'], ['translated', 'bpmn.import_column_after', 'width="26%"'],
        ['kindLabel', 'bpmn.import_column_kind'], ['element', 'bpmn.import_column_element'],
        ['warningChips', 'bpmn.import_column_warnings', 'renderer="html"']], 'bpmn.import_no_translations'));
      sections.push(section);
    }
    body.replaceChildren(...sections);
    renderFooter();
  }

  function renderFooter() {
    if (!loadable || loaded) {
      footer.innerHTML = `<tf-button variant="primary" data-act="close">${escapeHtml(I18n.t('bpmn.import_close'))}</tf-button>`;
    } else {
      footer.innerHTML = `<tf-button variant="ghost" data-act="close">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
        <tf-button variant="primary" icon="check" data-act="import" ${busy ? 'disabled' : ''}>${escapeHtml(I18n.t('bpmn.import_as_draft'))}</tf-button>`;
    }
  }

  win.addEventListener('click', async (event) => {
    const button = event.target.closest?.('[data-act]');
    if (!button || !win.contains(button)) return;
    if (button.dataset.act === 'close') { win.close(true); return; }
    if (button.dataset.act !== 'import' || busy || !loadable || loaded) return;
    busy = true;
    renderFooter();
    try { loaded = !!await onImport(); }
    finally { busy = false; }
    if (!win.isConnected) return;
    if (loaded) render();
    else win.close(true);
  });
  render();
  document.body.appendChild(win);
  return win;
}
