// =============================================================================
// File: modules/addon-app.js
// Opis: Renderer UI v2 dla addonow — drill-down screen `addon-app`.
// Pobiera tree_json przez ApiBinary.one('addonUiPanelGetRequest'), renderuje
// drzewo UiComponent przez tf-* komponenty + semantyczne elementy HTML.
// Button click / form submit -> addonUiActionRequest -> refresh panelu.
//
// Mapping UiComponent -> element (CLAUDE.md rule 8):
//   Input / Button / Select / Table / Tabs / Badge -> tf-* (primitive)
//   Text / Card / Image / List / Form / Divider / Progress / Code ->
//     semantyczny element HTML (div/section/img/ul/form/hr/progress/pre)
//
// Routing: Router.navigate('addon-app', { addonId, panelId }) -> show(params).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, byId } from '/js/utils.js';

const VIEW_ID = 'addon-app';

// =============================================================================
// Screen — drill-down (show(params), nie sidebar tile)
// =============================================================================

const AddonAppScreen = {
  async show(params = {}) {
    const addonId = String(params.addonId ?? params.addon_id ?? '');
    const panelId = String(params.panelId ?? params.panel_id ?? '');
    const main = byId('main');
    if (!main) return;

    if (!addonId || !panelId) {
      main.innerHTML = errorBlock('Brak parametrów addonId / panelId.');
      return;
    }

    main.innerHTML = `
      <div class="addon-app-shell" data-addon="${escapeHtml(addonId)}" data-panel="${escapeHtml(panelId)}">
        <div class="addon-app-loading">Ładowanie panelu…</div>
      </div>`;

    await refreshPanel(addonId, panelId);
  },
  unmount() {},
};

export default AddonAppScreen;
export { VIEW_ID };

// =============================================================================
// Fetch + render orchestration
// =============================================================================

async function refreshPanel(addonId, panelId) {
  const shell = document.querySelector('.addon-app-shell');
  if (!shell) return;

  let response;
  try {
    response = await ApiBinary.one('addonUiPanelGetRequest', { addonId, panelId });
  } catch (e) {
    shell.innerHTML = errorBlock(`Nie udało się pobrać panelu: ${e.message}`);
    return;
  }

  const treeJson = response?.treeJson ?? response?.tree_json ?? '';
  if (!treeJson) {
    shell.innerHTML = emptyBlock(addonId, panelId);
    return;
  }

  let panel;
  try {
    panel = JSON.parse(treeJson);
  } catch (e) {
    shell.innerHTML = errorBlock(`Panel UI ma niepoprawny JSON: ${e.message}`);
    return;
  }

  renderPanelInto(shell, panel, { addonId, panelId });
}

function renderPanelInto(root, panel, ctx) {
  const title = panel?.title ?? '';
  const components = Array.isArray(panel?.components) ? panel.components : [];

  root.innerHTML = '';
  const header = document.createElement('h1');
  header.className = 'addon-app-title';
  header.textContent = title;
  root.appendChild(header);

  const body = document.createElement('div');
  body.className = 'addon-app-body';
  root.appendChild(body);

  for (const c of components) {
    const node = renderComponent(c, ctx);
    if (node) body.appendChild(node);
  }
}

// =============================================================================
// Renderowanie pojedynczego komponentu — dispatcher po `type`
// =============================================================================

function renderComponent(c, ctx) {
  if (!c || typeof c !== 'object' || typeof c.type !== 'string') {
    return renderUnknown('<brak typu>');
  }
  switch (c.type) {
    case 'text':     return renderText(c);
    case 'input':    return renderInput(c, ctx);
    case 'button':   return renderButton(c, ctx);
    case 'select':   return renderSelect(c, ctx);
    case 'table':    return renderTable(c);
    case 'card':     return renderCard(c, ctx);
    case 'tabs':     return renderTabs(c, ctx);
    case 'image':    return renderImage(c);
    case 'list':     return renderList(c, ctx);
    case 'form':     return renderForm(c, ctx);
    case 'divider':  return renderDivider();
    case 'progress': return renderProgress(c);
    case 'code':     return renderCode(c);
    case 'badge':    return renderBadge(c);
    default:         return renderUnknown(c.type);
  }
}

// =============================================================================
// Primitives (tf-* komponenty wymagane przez rule 8)
// =============================================================================

function renderInput(c, ctx) {
  const el = document.createElement('tf-input');
  if (c.label) el.setAttribute('label', c.label);
  if (c.input_type) el.setAttribute('type', c.input_type);
  if (c.value != null) el.setAttribute('value', String(c.value));
  if (c.placeholder) el.setAttribute('placeholder', c.placeholder);
  if (c.id) {
    el.setAttribute('name', c.id);
    el.dataset.fieldId = c.id;
  }
  return el;
}

function renderButton(c, ctx) {
  const el = document.createElement('tf-button');
  el.setAttribute('label', c.label ?? '');
  if (c.style) el.setAttribute('variant', c.style);
  if (c.id) el.setAttribute('id', `addon-btn-${c.id}`);
  el.addEventListener('click', (ev) => {
    ev.preventDefault();
    const enclosingForm = el.closest('form[data-form-id]');
    dispatchAction(ctx, enclosingForm, c.action, {});
  });
  return el;
}

function renderSelect(c, ctx) {
  // tf-select wraps native <select> and reads <option> children from light
  // DOM (zob. components/tf-select.js). Generujemy options jako children,
  // NIE jako JSON attribute.
  const el = document.createElement('tf-select');
  if (c.id) {
    el.setAttribute('name', c.id);
    el.dataset.fieldId = c.id;
  }
  if (c.selected != null) el.setAttribute('value', String(c.selected));
  const options = Array.isArray(c.options) ? c.options : [];
  for (const pair of options) {
    const opt = document.createElement('option');
    const [value, display] = Array.isArray(pair) ? pair : [pair, pair];
    opt.value = String(value ?? '');
    opt.textContent = String(display ?? value ?? '');
    el.appendChild(opt);
  }
  // Label nie jest natywnym slotem tf-select — owijamy w <label> jesli podany.
  if (c.label) {
    const wrap = document.createElement('label');
    wrap.className = 'addon-select-wrap';
    const lbl = document.createElement('span');
    lbl.className = 'addon-select-label';
    lbl.textContent = c.label;
    wrap.appendChild(lbl);
    wrap.appendChild(el);
    return wrap;
  }
  return el;
}

function renderTable(c) {
  // tf-table API (z components/tf-table.js):
  // - <tf-column key label> jako children (light DOM)
  // - .rows = [{ key: val, ... }] jako property po dodaniu do DOM
  // Klucze kolumn = nagłówki (addon nie zna konkretnych key-ów; uzywamy
  // label jako kluczy ze "col-N" fallbackiem gdy nazwa pusta).
  const el = document.createElement('tf-table');
  const headers = Array.isArray(c.headers) ? c.headers : [];
  const rows = Array.isArray(c.rows) ? c.rows : [];

  const keys = headers.map((h, i) => (typeof h === 'string' && h.length > 0 ? h : `col-${i}`));
  keys.forEach((key, i) => {
    const col = document.createElement('tf-column');
    col.setAttribute('key', key);
    col.setAttribute('label', String(headers[i] ?? ''));
    el.appendChild(col);
  });

  const rowsData = rows.map((row) => {
    const obj = {};
    for (let i = 0; i < keys.length; i++) {
      obj[keys[i]] = row[i] ?? '';
    }
    return obj;
  });
  // .rows musi byc property po connectedCallback. Setter wywoluje _render
  // wewnatrz. Defer do mikrozadania zeby tf-table mial szanse sie zlinkowac.
  queueMicrotask(() => {
    el.rows = rowsData;
  });
  return el;
}

function renderTabs(c, ctx) {
  // tf-tabs renderuje tylko nav (children <tf-tab>). Content per zakladke
  // trzymamy w osobnym kontenerze i przelaczamy display przez `value`
  // event (zob. components/tf-tabs.js).
  const wrap = document.createElement('div');
  wrap.className = 'addon-tabs';

  const tabsArr = Array.isArray(c.tabs) ? c.tabs : [];
  const tabsNav = document.createElement('tf-tabs');
  const firstId = tabsArr.length > 0 ? 't0' : null;
  if (firstId) tabsNav.setAttribute('value', firstId);

  const panes = document.createElement('div');
  panes.className = 'addon-tabs-panes';

  tabsArr.forEach(([label, children], idx) => {
    const tabId = `t${idx}`;

    const tab = document.createElement('tf-tab');
    tab.id = tabId;
    tab.textContent = String(label ?? `Tab ${idx + 1}`);
    tabsNav.appendChild(tab);

    const pane = document.createElement('div');
    pane.className = 'addon-tab-pane';
    pane.dataset.tabPane = tabId;
    if (tabId !== firstId) pane.hidden = true;
    const arr = Array.isArray(children) ? children : [];
    for (const child of arr) {
      const node = renderComponent(child, ctx);
      if (node) pane.appendChild(node);
    }
    panes.appendChild(pane);
  });

  // tf-tabs emituje `value` change przez setAttribute('value', id). Sluchamy
  // przez MutationObserver bo native attribute change nie jest event'em.
  // Alternatywnie nasluchujemy `tf-tab-click` (custom event bubble z child).
  tabsNav.addEventListener('tf-tab-click', (ev) => {
    const activeId = ev.detail?.id;
    if (!activeId) return;
    panes.querySelectorAll('[data-tab-pane]').forEach((p) => {
      p.hidden = p.dataset.tabPane !== activeId;
    });
  });

  wrap.appendChild(tabsNav);
  wrap.appendChild(panes);
  return wrap;
}

function renderBadge(c) {
  // tf-chip czyta text z innerHTML (NIE z attribute label) i ma atrybut
  // `status` zamiast `variant`. Mapujemy color z UiComponent::Badge na
  // status — addonowe palety zwykle uzywaja green/red/yellow/blue;
  // mapujemy je na canonical {success, danger, warning, info}.
  const el = document.createElement('tf-chip');
  el.textContent = c.text ?? '';
  const color = (c.color ?? '').toLowerCase();
  const status = ({
    green: 'success',
    success: 'success',
    red: 'danger',
    danger: 'danger',
    error: 'danger',
    yellow: 'warning',
    orange: 'warning',
    warning: 'warning',
    blue: 'info',
    info: 'info',
  })[color] || 'info';
  el.setAttribute('status', status);
  return el;
}

// =============================================================================
// Semantic / layout containers (nie primitive — native HTML)
// =============================================================================

function renderText(c) {
  const el = document.createElement('div');
  el.className = 'addon-text';
  if (c.style) el.setAttribute('style', String(c.style));
  el.textContent = c.content ?? '';
  return el;
}

function renderCard(c, ctx) {
  const card = document.createElement('section');
  card.className = 'addon-card';
  if (c.title) {
    const h = document.createElement('h3');
    h.className = 'addon-card-title';
    h.textContent = c.title;
    card.appendChild(h);
  }
  const body = document.createElement('div');
  body.className = 'addon-card-body';
  const children = Array.isArray(c.children) ? c.children : [];
  for (const child of children) {
    const node = renderComponent(child, ctx);
    if (node) body.appendChild(node);
  }
  card.appendChild(body);
  return card;
}

function renderImage(c) {
  const el = document.createElement('img');
  el.className = 'addon-image';
  el.setAttribute('src', c.src ?? '');
  el.setAttribute('alt', c.alt ?? '');
  if (c.width) el.setAttribute('width', String(c.width));
  if (c.height) el.setAttribute('height', String(c.height));
  return el;
}

function renderList(c, ctx) {
  const el = document.createElement('ul');
  el.className = 'addon-list';
  const items = Array.isArray(c.items) ? c.items : [];
  for (const item of items) {
    const li = document.createElement('li');
    const node = renderComponent(item, ctx);
    if (node) li.appendChild(node);
    el.appendChild(li);
  }
  return el;
}

function renderForm(c, ctx) {
  const form = document.createElement('form');
  form.className = 'addon-form';
  form.dataset.formId = c.id ?? '';
  const children = Array.isArray(c.children) ? c.children : [];
  for (const child of children) {
    const node = renderComponent(child, ctx);
    if (node) form.appendChild(node);
  }

  const submit = document.createElement('tf-button');
  submit.setAttribute('label', 'Wyślij');
  submit.setAttribute('variant', 'primary');
  submit.setAttribute('type', 'submit');
  form.appendChild(submit);

  form.addEventListener('submit', (ev) => {
    ev.preventDefault();
    dispatchAction(ctx, form, c.submit_action, {});
  });
  // tf-button generuje click — przechwytujemy zeby wyzwolic submit handler
  submit.addEventListener('click', (ev) => {
    ev.preventDefault();
    form.dispatchEvent(new Event('submit', { cancelable: true }));
  });
  return form;
}

function renderDivider() {
  const hr = document.createElement('hr');
  hr.className = 'addon-divider';
  return hr;
}

function renderProgress(c) {
  const wrap = document.createElement('div');
  wrap.className = 'addon-progress';
  const bar = document.createElement('progress');
  bar.className = 'addon-progress-bar';
  const value = clamp01(Number(c.value) || 0);
  bar.setAttribute('max', '1');
  bar.setAttribute('value', String(value));
  wrap.appendChild(bar);
  const label = document.createElement('span');
  label.className = 'addon-progress-label';
  label.textContent = c.label ?? `${Math.round(value * 100)}%`;
  wrap.appendChild(label);
  return wrap;
}

function renderCode(c) {
  const pre = document.createElement('pre');
  pre.className = 'addon-code';
  const code = document.createElement('code');
  code.className = `language-${(c.language ?? 'plain').replace(/[^a-z0-9_-]/gi, '')}`;
  code.textContent = c.content ?? '';
  pre.appendChild(code);
  return pre;
}

function renderUnknown(typeName) {
  const el = document.createElement('div');
  el.className = 'addon-unknown';
  el.textContent = `[unknown component: ${typeName}]`;
  return el;
}

// =============================================================================
// Action dispatch — zbiera form values, wysyla addonUiActionRequest,
// po sukcesie refetchuje panel (addon mogl zaktualizowac UI przez ui_render).
// =============================================================================

async function dispatchAction(ctx, formEl, actionId, extraParams) {
  if (!actionId) return;
  const params = formEl ? collectFormValues(formEl) : {};
  Object.assign(params, extraParams || {});

  try {
    await ApiBinary.one('addonUiActionRequest', {
      addonId: ctx.addonId,
      panelId: ctx.panelId,
      actionId,
      params,
    });
  } catch (e) {
    console.error('[addon-app] action dispatch failed:', e);
    // Surface bledu w panelu — nie hydrujemy refetcha bo addon byl niemoc'a.
    const shell = document.querySelector('.addon-app-shell');
    if (shell) {
      const banner = document.createElement('div');
      banner.className = 'addon-action-error';
      banner.textContent = `Akcja "${actionId}" nie powiodla sie: ${e.message}`;
      shell.insertBefore(banner, shell.firstChild);
    }
    return;
  }

  // Po akcji — addon mogl wywolac ui_render z nowym tree. Refetch.
  await refreshPanel(ctx.addonId, ctx.panelId);
}

function collectFormValues(formEl) {
  const out = {};
  if (!formEl) return out;
  // tf-input / tf-select eksponuja swoje value przez attribute `value`
  // (tak jak normalne form controls). Zbieramy po data-field-id.
  formEl.querySelectorAll('[data-field-id]').forEach((el) => {
    const id = el.dataset.fieldId;
    if (!id) return;
    let v;
    // Web components typowo eksponuja value jako property na elemencie
    if ('value' in el) v = el.value;
    else v = el.getAttribute('value') ?? '';
    out[id] = v;
  });
  return out;
}

// =============================================================================
// Helpers
// =============================================================================

function clamp01(n) {
  if (Number.isNaN(n)) return 0;
  if (n < 0) return 0;
  if (n > 1) return 1;
  return n;
}

function errorBlock(msg) {
  return `<div class="addon-app-error"><h3>Błąd</h3><p>${escapeHtml(msg)}</p></div>`;
}

function emptyBlock(addonId, panelId) {
  return `<div class="addon-app-empty">
    <h3>Panel nie został jeszcze wyrenderowany</h3>
    <p>Addon <code>${escapeHtml(addonId)}</code> nie zapisał drzewa UI dla panelu
      <code>${escapeHtml(panelId)}</code>. Sprawdź czy <code>on_start</code>
      lub <code>on_request</code> woła <code>ui_render</code>.</p>
  </div>`;
}
