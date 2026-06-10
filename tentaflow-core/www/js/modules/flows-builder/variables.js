// =============================================================================
// Plik: modules/flows-builder/variables.js
// Opis: Edytor deklaracji zmiennych flow (§3.12 / R10). Prosty panel w oknie
//       tf-window: tabela {name, type, default?, description?} z dodawaniem i
//       usuwaniem wierszy. Backend parsuje te deklaracje z sekcji `variables`
//       flow_json (FlowDefinition.variables) — output_mapping moze pisac tylko
//       do zadeklarowanej zmiennej. Pelny edytor io-mapping per node przyjdzie
//       w fazie 7; tu deklarujemy wylacznie same zmienne.
// =============================================================================

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { TfWindow } from '/js/components/tf-window.js';
import { I18n } from '/js/i18n.js';

// Typy zgodne z FlowDataType (snake_case w wire). `any` jest domyslny — round-
// trippuje legacy flow_json byte-identycznie.
const VAR_TYPES = ['any', 'text', 'json', 'audio', 'image', 'video', 'embedding', 'other'];

// kebab/snake-case, zaczyna sie litera, <=64 znaki. Backend nie wymusza wzorca
// nazwy zmiennej, ale CEL adresuje je jako `vars.<name>`, wiec nazwa musi byc
// legalnym identyfikatorem.
const NAME_RE = /^[a-z][a-z0-9_]{0,63}$/;

/// Otwiera modal edycji zmiennych. `current` to tablica deklaracji
/// {name, type, default?, description?}. Resolves z nowa tablica gdy uzytkownik
/// zapisze, albo `null` gdy anuluje (caller nie zmienia stanu).
export function openVariablesEditor(current = []) {
  // Praca na kopii — anulowanie nie moze zmutowac stanu wolajacego.
  const rows = current.map((v) => ({
    name: v.name ?? '',
    type: VAR_TYPES.includes(v.type) ? v.type : 'any',
    // default i description przechowujemy jako tekst w UI; przy zapisie
    // default parsujemy jako JSON (z fallbackiem na string).
    defaultText: v.default === undefined || v.default === null ? '' : stringifyDefault(v.default),
    description: v.description ?? '',
  }));

  const body = document.createElement('div');
  body.className = 'fb-vars-editor';
  body.style.display = 'flex';
  body.style.flexDirection = 'column';
  body.style.gap = '12px';
  body.style.minWidth = '560px';
  body.style.maxWidth = '720px';

  const hint = document.createElement('p');
  hint.style.margin = '0';
  hint.style.fontSize = '12px';
  hint.style.color = 'var(--tf-text-3)';
  hint.textContent = I18n.t('flows_vars.hint');
  body.appendChild(hint);

  const table = document.createElement('div');
  table.className = 'fb-vars-table';
  table.setAttribute('role', 'table');
  body.appendChild(table);

  const errorEl = document.createElement('div');
  errorEl.className = 'fb-vars-error';
  errorEl.style.color = 'var(--tf-danger, #e5484d)';
  errorEl.style.fontSize = '12px';
  errorEl.style.minHeight = '16px';
  body.appendChild(errorEl);

  const addBtn = document.createElement('tf-button');
  addBtn.setAttribute('variant', 'secondary');
  addBtn.setAttribute('size', 'sm');
  addBtn.setAttribute('icon', 'plus');
  addBtn.textContent = I18n.t('flows_vars.add');
  body.appendChild(addBtn);

  const renderRows = () => {
    table.innerHTML = `
      <div class="fb-vars-row fb-vars-head" role="row">
        <span>${escapeHtml(I18n.t('flows_vars.col_name'))}</span>
        <span>${escapeHtml(I18n.t('flows_vars.col_type'))}</span>
        <span>${escapeHtml(I18n.t('flows_vars.col_default'))}</span>
        <span>${escapeHtml(I18n.t('flows_vars.col_description'))}</span>
        <span></span>
      </div>
      ${rows.length === 0 ? `<div class="fb-vars-empty">${escapeHtml(I18n.t('flows_vars.empty'))}</div>` : ''}
    `;
    rows.forEach((row, idx) => {
      const el = document.createElement('div');
      el.className = 'fb-vars-row';
      el.setAttribute('role', 'row');
      el.dataset.idx = String(idx);
      const typeOptions = VAR_TYPES.map(
        (t) => `<option value="${escapeAttr(t)}"${t === row.type ? ' selected' : ''}>${escapeHtml(t)}</option>`,
      ).join('');
      el.innerHTML = `
        <tf-input data-field="name" value="${escapeAttr(row.name)}" placeholder="${escapeAttr(I18n.t('flows_vars.name_placeholder'))}"></tf-input>
        <tf-select data-field="type" value="${escapeAttr(row.type)}">${typeOptions}</tf-select>
        <tf-input data-field="defaultText" value="${escapeAttr(row.defaultText)}" placeholder="${escapeAttr(I18n.t('flows_vars.default_placeholder'))}"></tf-input>
        <tf-input data-field="description" value="${escapeAttr(row.description)}" placeholder="${escapeAttr(I18n.t('flows_vars.description_placeholder'))}"></tf-input>
        <tf-button variant="ghost" size="sm" icon="trash" data-action="remove" title="${escapeAttr(I18n.t('flows_vars.remove'))}"></tf-button>
      `;
      table.appendChild(el);
    });
  };

  // Czytaj wartosci z pol z powrotem do `rows` (live, przy kazdej edycji).
  const syncRow = (el) => {
    const idx = Number(el.dataset.idx);
    const row = rows[idx];
    if (!row) return;
    el.querySelectorAll('[data-field]').forEach((field) => {
      const key = field.dataset.field;
      row[key] = field.value ?? '';
    });
  };

  table.addEventListener('input', (ev) => {
    const rowEl = ev.target.closest('.fb-vars-row');
    if (rowEl) syncRow(rowEl);
  });
  table.addEventListener('change', (ev) => {
    const rowEl = ev.target.closest('.fb-vars-row');
    if (rowEl) syncRow(rowEl);
  });
  table.addEventListener('click', (ev) => {
    const btn = ev.target.closest('[data-action="remove"]');
    if (!btn) return;
    const rowEl = btn.closest('.fb-vars-row');
    const idx = Number(rowEl?.dataset.idx);
    if (Number.isInteger(idx)) {
      rows.splice(idx, 1);
      renderRows();
    }
  });

  addBtn.addEventListener('click', () => {
    rows.push({ name: '', type: 'any', defaultText: '', description: '' });
    renderRows();
  });

  renderRows();

  const footer = document.createElement('div');
  footer.innerHTML = `
    <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('flows_vars.cancel'))}</tf-button>
    <tf-button variant="primary" icon="check" data-action="save">${escapeHtml(I18n.t('flows_vars.save'))}</tf-button>
  `;

  return new Promise((resolve) => {
    let settled = false;
    const win = document.createElement('tf-window');
    win.setAttribute('title', I18n.t('flows_vars.title'));
    win.setAttribute('icon', 'code');
    win.setAttribute('buttons', 'close');
    win.setAttribute('width', '680');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');
    const bWrap = document.createElement('div'); bWrap.slot = 'body'; bWrap.appendChild(body);
    const fWrap = document.createElement('div'); fWrap.slot = 'footer'; fWrap.appendChild(footer);
    win.appendChild(bWrap); win.appendChild(fWrap);
    const backdrop = document.createElement('div');
    backdrop.className = 'tf-window-backdrop';
    document.body.appendChild(backdrop);
    document.body.appendChild(win);

    const cleanup = (result) => {
      if (settled) return;
      settled = true;
      if (win.isConnected) win.remove();
      if (backdrop.isConnected) backdrop.remove();
      resolve(result);
    };

    const commit = () => {
      // Re-sync wszystkich wierszy (pewnosc, nie tylko ostatnio edytowany).
      table.querySelectorAll('.fb-vars-row[data-idx]').forEach(syncRow);
      const validated = validateRows(rows);
      if (validated.error) {
        errorEl.textContent = validated.error;
        return;
      }
      cleanup(validated.declarations);
    };

    footer.addEventListener('click', (ev) => {
      if (ev.target.closest('[data-action="save"]')) commit();
      else if (ev.target.closest('[data-action="cancel"]')) cleanup(null);
    });
    win.addEventListener('action', (ev) => {
      if (ev.detail?.action === 'close') cleanup(null);
    });
    backdrop.addEventListener('click', () => cleanup(null));
  });
}

// Serializuje `default` do tekstu edytowalnego: stringi bez cudzyslowow,
// reszta jako JSON (liczby, bool, obiekty, tablice).
function stringifyDefault(value) {
  if (typeof value === 'string') return value;
  try {
    return JSON.stringify(value);
  } catch (_) {
    return String(value);
  }
}

// Parsuje tekst default na FlowValue-friendly JSON: probuje JSON.parse, a gdy
// to nie liczba/bool/obiekt/tablica/null traktuje jako goly string.
function parseDefault(text) {
  const trimmed = text.trim();
  if (trimmed === '') return undefined;
  try {
    return JSON.parse(trimmed);
  } catch (_) {
    return text;
  }
}

// Waliduje wiersze, zwraca {declarations} albo {error}. Wymusza unikalne,
// poprawne nazwy. Default i description sa opcjonalne.
function validateRows(rows) {
  const seen = new Set();
  const declarations = [];
  for (const row of rows) {
    const name = (row.name ?? '').trim();
    if (name === '') {
      return { error: I18n.t('flows_vars.err_empty_name') };
    }
    if (!NAME_RE.test(name)) {
      return { error: I18n.t('flows_vars.err_bad_name', { name }) };
    }
    if (seen.has(name)) {
      return { error: I18n.t('flows_vars.err_duplicate', { name }) };
    }
    seen.add(name);
    const decl = { name, type: VAR_TYPES.includes(row.type) ? row.type : 'any' };
    const def = parseDefault(row.defaultText ?? '');
    if (def !== undefined) decl.default = def;
    const desc = (row.description ?? '').trim();
    if (desc !== '') decl.description = desc;
    declarations.push(decl);
  }
  return { declarations };
}
