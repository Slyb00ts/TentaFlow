// ===== File: process-import.js — the window that reports what a Camunda import found before anything is loaded =====

import { escapeAttr, escapeHtml, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import '/js/components/tf-table.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import { processImportReport, processImportCounts, processImportGroups, processImportLoadable, processImportSummaryText, processImportTranslations } from './bpmn.js';

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
export function openProcessImportReview({ response, onImport, onReveal, onBack, onDismiss, loaded: alreadyLoaded = false, returnFocus = null }) {
  const loadable = processImportLoadable(response);
  const counts = processImportCounts(response);
  const language = I18n.getLanguage();
  let loaded = alreadyLoaded;
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
  // The body takes focus when the window opens and scrolls from the keyboard; "Show" buttons and the footer follow in Tab order.
  body.tabIndex = 0;
  body.setAttribute('role', 'region');
  body.setAttribute('aria-label', I18n.t('bpmn.import_review_title'));
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
        [['message', 'bpmn.import_column_message', 'renderer="html" fill'], ['element', 'bpmn.import_column_element', 'width="22%"'],
          ['code', 'bpmn.import_column_code', 'width="30%"']], 'bpmn.import_no_diagnostics'));
      sections.push(section);
    }
    const translations = processImportTranslations(response.translations, model);
    if (translations.length) {
      const section = document.createElement('section');
      section.dataset.translations = '';
      section.innerHTML = `<h3>${escapeHtml(I18n.t('bpmn.import_translations_title'))}</h3>
        <p class="fb-field-hint">${escapeHtml(I18n.t('bpmn.import_translations_hint'))}</p>`;
      section.append(table(translations.map((row) => ({ ...row,
        // The explanation is written out, not behind a tooltip: touch and keyboard readers get it too.
        differences: row.warnings.map((warning) => `<div class="tf-table__cell-sub">${escapeHtml(warning.text)}</div>`).join('') })),
      [['original', 'bpmn.import_column_before', 'width="26%"'], ['translated', 'bpmn.import_column_after', 'width="26%"'],
        ['kindLabel', 'bpmn.import_column_kind'], ['element', 'bpmn.import_column_element'],
        ['differences', 'bpmn.import_column_warnings', 'renderer="html"']], 'bpmn.import_no_translations'));
      sections.push(section);
    }
    body.replaceChildren(...sections);
    renderFooter();
  }

  function renderFooter() {
    const copy = `<tf-button variant="ghost" icon="copy" data-act="copy">${escapeHtml(I18n.t('bpmn.import_copy_report'))}</tf-button>`;
    const back = !loaded && onBack ? `<tf-button variant="ghost" icon="arrow-left" data-act="back">${escapeHtml(I18n.t('bpmn.import_back_to_xml'))}</tf-button>` : '';
    if (!loadable || loaded) {
      footer.innerHTML = `${copy}${back}<tf-button variant="primary" data-act="close">${escapeHtml(I18n.t('bpmn.import_close'))}</tf-button>`;
    } else {
      footer.innerHTML = `${copy}${back}<tf-button variant="ghost" data-act="close">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
        <tf-button variant="primary" icon="check" data-act="import" ${busy ? 'disabled' : ''}>${escapeHtml(I18n.t('bpmn.import_as_draft'))}</tf-button>`;
    }
  }

  async function copyReport() {
    try {
      await navigator.clipboard.writeText(processImportReport(response));
      toast(I18n.t('bpmn.import_report_copied'), 'success');
    } catch {
      toast(I18n.t('bpmn.import_report_copy_failed'), 'error');
    }
  }

  win.addEventListener('click', async (event) => {
    const button = event.target.closest?.('[data-act]');
    if (!button || !win.contains(button)) return;
    if (button.dataset.act === 'close') { win.close(true); return; }
    if (button.dataset.act === 'copy') { copyReport(); return; }
    if (button.dataset.act === 'back') { win.close(true); onBack(); return; }
    if (button.dataset.act !== 'import' || busy || !loadable || loaded) return;
    busy = true;
    renderFooter();
    try { loaded = !!await onImport(); }
    catch {
      loaded = false;
      toast(I18n.t('bpmn.import_not_loaded'), 'error');
    } finally { busy = false; }
    if (!win.isConnected) return;
    // A refused replacement leaves the result open so the author can still read it or try again.
    if (loaded) render();
    else renderFooter();
  });
  // Escape or the backdrop would drop a result nobody has acted on; the buttons are the explicit ways out.
  let asking = false;
  win.addEventListener('close-request', async (event) => {
    if (!loadable || loaded || asking) return;
    event.preventDefault();
    asking = true;
    const discard = await TfWindow.confirm({ title: I18n.t('bpmn.import_result_discard_title'), message: I18n.t('bpmn.import_result_discard_hint'),
      confirmLabel: I18n.t('bpmn.import_discard'), cancelLabel: I18n.t('bpmn.import_keep_result'), danger: true });
    asking = false;
    if (discard && win.isConnected) win.close(true);
  });
  // A language switch remounts the screen and closes every overlay; the pending import is the caller's to reopen.
  win.addEventListener('closed', () => {
    if (I18n.getLanguage() === language) onDismiss?.();
    if (returnFocus?.isConnected) returnFocus.focus?.();
  });
  render();
  document.body.appendChild(win);
  body.focus({ preventScroll: true });
  return win;
}
