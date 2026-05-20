// =============================================================================
// File: modules/addon-app.js
// Opis: Renderer UI v2 dla addonow. Obsluguje pelne PanelTree z SDK (Chunk 2.7):
//   - PanelTree { root, overlays, navigation } — nowy ksztalt
//   - Legacy { components: [...] } — backward-compat sprzed Chunka 2.1
// Sub-enumy UiComponent (Layout/Container/DataDisplay/Form/Feedback/Action/
// Specialized/Legacy) dispatchowane po `type` (snake_case lub `_v2` dla kolizji).
//
// Eventy: kazdy on_* handler addona przechodzi przez `dispatchAction` ->
// `addonUiActionRequest`; nawigacja przez NavTabs/Sidebar/Link/Breadcrumb idzie
// przez `Router.navigate('addon-app', { addonId, panelId })`.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, byId } from '/js/utils.js';
import {
  panelTransition,
  animateNumber,
  parseStatValue,
  staggerEnter,
  animatedRemove,
  prefersReducedMotion,
} from '/js/modules/motion.js';
import { renderIcon } from '/js/modules/sdk-icons.js';
import '/js/components/tf-canvas.js';
import '/js/components/tf-sparkline.js';
import '/js/components/tf-heatmap.js';
import '/js/components/tf-alarm-feed.js';
import '/js/components/tf-fps-counter.js';

const VIEW_ID = 'addon-app';

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
  unmount() {
    // Czyscimy overlay portale ktore moglyby przeciekac do innego widoku
    document.querySelectorAll('[data-sdk-overlay-portal]').forEach((n) => n.remove());
  },
};

export default AddonAppScreen;
export { VIEW_ID };

// =============================================================================
// Fetch + render orchestration
// =============================================================================

async function refreshPanel(addonId, panelId) {
  const shell = document.querySelector('.addon-app-shell');
  if (!shell) return;

  const existing = shell.querySelector(':scope > .addon-app-content');
  // Pierwszy render albo loading placeholder — wstawiamy bez tranzycji.
  if (!existing) {
    const content = await fetchAndBuildContent(addonId, panelId);
    shell.innerHTML = '';
    shell.appendChild(content);
    return;
  }

  // Kolejny render — fade-out starego, fetch nowego, fade-in.
  await panelTransition(existing, () => fetchAndBuildContent(addonId, panelId));
}

async function fetchAndBuildContent(addonId, panelId) {
  const content = document.createElement('div');
  content.className = 'addon-app-content';

  let response;
  try {
    response = await ApiBinary.one('addonUiPanelGetRequest', { addonId, panelId });
  } catch (e) {
    content.innerHTML = errorBlock(`Nie udało się pobrać panelu: ${e.message}`);
    return content;
  }

  const treeJson = response?.treeJson ?? response?.tree_json ?? '';
  if (!treeJson) {
    content.innerHTML = emptyBlock(addonId, panelId);
    return content;
  }

  let panel;
  try {
    panel = JSON.parse(treeJson);
  } catch (e) {
    content.innerHTML = errorBlock(`Panel UI ma niepoprawny JSON: ${e.message}`);
    return content;
  }

  renderPanelInto(content, panel, { addonId, panelId });
  return content;
}

function renderPanelInto(root, panel, ctx) {
  // Usun stare overlay portale przed nowym renderingiem panelu
  document.querySelectorAll('[data-sdk-overlay-portal]').forEach((n) => n.remove());

  const title = panel?.title ?? '';
  // PanelTree v2 ma `root: [...]`; legacy ma `components: [...]`.
  const components = Array.isArray(panel?.root)
    ? panel.root
    : (Array.isArray(panel?.components) ? panel.components : []);
  const overlays = Array.isArray(panel?.overlays) ? panel.overlays : [];
  const navigation = panel?.navigation ?? null;

  root.innerHTML = '';
  if (title) {
    const header = document.createElement('h1');
    header.className = 'addon-app-title';
    header.textContent = title;
    root.appendChild(header);
  }

  const panelEl = document.createElement('div');
  panelEl.className = navigation?.sidebar ? 'addon-panel addon-panel-with-sidebar' : 'addon-panel';

  // Sidebar nawigacji jesli zadeklarowano
  if (navigation?.sidebar) {
    panelEl.appendChild(renderNavigationSidebar(navigation, ctx));
  }

  const main = document.createElement('main');
  main.className = 'addon-panel-main';
  // Breadcrumb na gorze maina jezeli jest
  if (navigation?.breadcrumb) {
    main.appendChild(renderBreadcrumb({ items: navigation.breadcrumb.items }, ctx));
  }
  for (const c of components) {
    const node = renderComponent(c, ctx);
    if (node) main.appendChild(node);
  }
  panelEl.appendChild(main);
  root.appendChild(panelEl);

  // Overlay portale (window/drawer/popover) trafiaja do body, nie do shell
  for (const overlay of overlays) {
    if (overlay && overlay.visible !== false) {
      const portal = createOverlayPortal(overlay, ctx);
      if (portal) document.body.appendChild(portal);
    }
  }
}

// =============================================================================
// Dispatcher — sub-enumy SDK + legacy
// =============================================================================

function renderComponent(c, ctx) {
  if (!c || typeof c !== 'object' || typeof c.type !== 'string') {
    return renderUnknown('<brak typu>');
  }
  switch (c.type) {
    // Layout
    case 'stack': return renderStack(c, ctx);
    case 'grid': return renderGrid(c, ctx);
    case 'spacer': return renderSpacer(c);
    case 'divider': return renderDivider(c);
    case 'split': return renderSplit(c, ctx);

    // Container
    case 'card': return renderCard(c, ctx);
    case 'section_card': return renderSectionCard(c, ctx);
    case 'section': return renderSection(c, ctx);
    case 'tabs': return renderTabs(c, ctx);
    case 'nav_tabs': return renderNavTabs(c, ctx);
    case 'toolbar': return renderToolbar(c, ctx);
    case 'sidebar': return renderSidebarContainer(c, ctx);
    case 'collapsible': return renderCollapsible(c, ctx);
    case 'tooltip': return renderTooltip(c, ctx);
    case 'breadcrumb': return renderBreadcrumb(c, ctx);
    case 'pagination': return renderPagination(c, ctx);
    // Window/Drawer/Popover w root traktujemy jako warning — backend powinien
    // odrzucic, ale renderer nie jest autorytetem. Pokazujemy unknown.
    case 'window':
    case 'drawer':
    case 'popover': return renderUnknown(`${c.type} (musi byc w overlays)`);

    // DataDisplay
    case 'text_v2': return renderTextV2(c);
    case 'heading': return renderHeading(c);
    case 'badge_v2': return renderBadgeV2(c);
    case 'chip': return renderChipV2(c, ctx);
    case 'tag': return renderTag(c);
    case 'avatar': return renderAvatar(c);
    case 'image_v2': return renderImageV2(c);
    case 'stat': return renderStat(c);
    case 'key_value': return renderKeyValue(c, ctx);
    case 'list_v2': return renderListV2(c, ctx);
    case 'bullet_list': return renderBulletList(c);
    case 'timeline': return renderTimeline(c);
    case 'table_v2': return renderTableV2(c, ctx);
    case 'mono_block': return renderMonoBlock(c);
    case 'code_block': return renderCodeBlock(c);
    case 'empty_state': return renderEmptyState(c, ctx);

    // Form
    case 'input_v2': return renderInputV2(c, ctx);
    case 'textarea': return renderTextarea(c, ctx);
    case 'select_v2': return renderSelectV2(c, ctx);
    case 'multi_select': return renderMultiSelect(c, ctx);
    case 'checkbox': return renderCheckbox(c, ctx);
    case 'radio': return renderRadio(c, ctx);
    case 'radio_group': return renderRadioGroup(c, ctx);
    case 'radio_card_group': return renderRadioCardGroup(c, ctx);
    case 'toggle': return renderToggleV2(c, ctx);
    case 'slider': return renderSlider(c, ctx);
    case 'slider_row': return renderSliderRow(c, ctx);
    case 'date_picker': return renderDatePicker(c, ctx);
    case 'date_range_picker': return renderDateRangePicker(c, ctx);
    case 'time_picker': return renderTimePicker(c, ctx);
    case 'file_upload': return renderFileUpload(c, ctx);
    case 'search_v2': return renderSearchV2(c, ctx);
    case 'form_v2': return renderFormV2(c, ctx);
    case 'form_field': return renderFormField(c, ctx);
    case 'form_group': return renderFormGroup(c, ctx);

    // Feedback
    case 'alert': return renderAlert(c, ctx);
    case 'banner': return renderBanner(c, ctx);
    case 'callout': return renderCallout(c, ctx);
    case 'toast': return renderToastInline(c, ctx);
    case 'spinner': return renderSpinner(c);
    case 'progress_v2': return renderProgressV2(c);
    case 'skeleton': return renderSkeleton(c);
    case 'hint': return renderHint(c);
    case 'gate_screen': return renderGateScreen(c, ctx);

    // Action
    case 'button_v2': return renderButtonV2(c, ctx);
    case 'icon_button': return renderIconButton(c, ctx);
    case 'button_group': return renderButtonGroup(c, ctx);
    case 'link': return renderLink(c, ctx);
    case 'menu': return renderMenu(c, ctx);
    case 'action_bar': return renderActionBar(c, ctx);
    case 'filter_chips': return renderFilterChips(c, ctx);
    case 'wizard_footer': return renderWizardFooter(c, ctx);

    // Specialized
    case 'canvas': return renderCanvasSpecialized(c, ctx);
    case 'sparkline': return renderSparkline(c);
    case 'stacked_bar': return renderStackedBar(c);
    case 'heatmap': return renderHeatmapSpecialized(c, ctx);
    case 'access_matrix': return renderAccessMatrix(c, ctx);
    case 'weekly_schedule_grid': return renderWeeklyScheduleGrid(c, ctx);
    case 'video_tile': return renderVideoTile(c, ctx);
    case 'welcome_hero': return renderWelcomeHero(c, ctx);
    case 'step_progress': return renderStepProgress(c);
    case 'req_card': return renderReqCard(c, ctx);
    case 'decision_row': return renderDecisionRow(c, ctx);
    case 'alarm_feed': return renderAlarmFeed(c, ctx);
    case 'fps_counter': return renderFpsCounter(c);

    // Legacy
    case 'text':     return renderLegacyText(c);
    case 'input':    return renderLegacyInput(c, ctx);
    case 'button':   return renderLegacyButton(c, ctx);
    case 'select':   return renderLegacySelect(c, ctx);
    case 'table':    return renderLegacyTable(c);
    case 'tabs_legacy': return renderLegacyTabs(c, ctx);
    case 'image':    return renderLegacyImage(c);
    case 'list':     return renderLegacyList(c, ctx);
    case 'form':     return renderLegacyForm(c, ctx);
    case 'progress': return renderLegacyProgress(c);
    case 'code':     return renderLegacyCode(c);
    case 'badge':    return renderLegacyBadge(c);
    case 'live_camera_tile': return renderLiveCameraTile(c, ctx);
    case 'video_stream': return renderVideoStream(c);

    default: return renderUnknown(c.type);
  }
}

// =============================================================================
// Layout
// =============================================================================

function spacingVar(token) {
  if (!token) return '0';
  return `var(--sdk-spacing-${token})`;
}

function renderStack(c, ctx) {
  const el = document.createElement('div');
  el.className = `sdk-stack sdk-stack-${c.direction === 'horizontal' ? 'horizontal' : 'vertical'}`;
  if (c.wrap) el.classList.add('sdk-stack-wrap');
  el.style.gap = spacingVar(c.gap || 'md');
  if (c.padding) el.style.padding = spacingVar(c.padding);
  el.style.alignItems = mapAlign(c.align);
  el.style.justifyContent = mapJustify(c.justify);
  for (const child of c.children || []) {
    const node = renderComponent(child, ctx);
    if (node) el.appendChild(node);
  }
  return el;
}

function mapAlign(a) {
  switch (a) {
    case 'start': return 'flex-start';
    case 'end': return 'flex-end';
    case 'center': return 'center';
    case 'baseline': return 'baseline';
    case 'stretch':
    default: return 'stretch';
  }
}

function mapJustify(j) {
  switch (j) {
    case 'center': return 'center';
    case 'end': return 'flex-end';
    case 'space_between': return 'space-between';
    case 'space_around': return 'space-around';
    case 'space_evenly': return 'space-evenly';
    case 'start':
    default: return 'flex-start';
  }
}

function sizeToCss(size) {
  if (!size || typeof size !== 'object') return 'auto';
  switch (size.kind) {
    case 'auto': return 'auto';
    case 'fill': return '1fr';
    case 'fr': return `${Number(size.value) || 1}fr`;
    case 'percent': return `${Number(size.value) || 0}%`;
    case 'fixed': return sizeUnitToCss(size.unit);
    case 'min_max': return `minmax(${sizeUnitToCss(size.min)}, ${sizeUnitToCss(size.max)})`;
    default: return 'auto';
  }
}

function sizeUnitToCss(unit) {
  if (!unit) return 'auto';
  if (unit.kind === 'px') return `${Number(unit.value) || 0}px`;
  if (unit.kind === 'spacing') return spacingVar(unit.value || 'md');
  return 'auto';
}

function trackToCss(track) {
  if (!track) return 'auto';
  switch (track.kind) {
    case 'repeat': return `repeat(${Number(track.count) || 1}, ${sizeToCss(track.size)})`;
    case 'explicit': return (track.tracks || []).map(sizeToCss).join(' ');
    case 'auto_fill': return `repeat(auto-fill, minmax(${sizeUnitToCss(track.min)}, ${sizeUnitToCss(track.max)}))`;
    default: return 'auto';
  }
}

function renderGrid(c, ctx) {
  const el = document.createElement('div');
  el.className = 'sdk-grid';
  el.style.gap = spacingVar(c.gap || 'md');
  el.style.gridTemplateColumns = trackToCss(c.columns);
  el.style.gridTemplateRows = trackToCss(c.rows);
  if (Array.isArray(c.areas) && c.areas.length > 0) {
    el.style.gridTemplateAreas = c.areas.map((row) => `"${row.join(' ')}"`).join(' ');
  }
  for (const item of c.children || []) {
    const child = renderComponent(item.component, ctx);
    if (!child) continue;
    if (item.area) child.style.gridArea = item.area;
    if (item.column_start) child.style.gridColumnStart = String(item.column_start);
    if (item.column_end) child.style.gridColumnEnd = String(item.column_end);
    if (item.row_start) child.style.gridRowStart = String(item.row_start);
    if (item.row_end) child.style.gridRowEnd = String(item.row_end);
    el.appendChild(child);
  }
  return el;
}

function renderSpacer(c) {
  const el = document.createElement('span');
  el.className = 'sdk-spacer';
  const size = spacingVar(c.size || 'md');
  if (c.direction === 'horizontal') el.style.width = size; else el.style.height = size;
  return el;
}

function renderDivider(c) {
  const el = document.createElement(c.direction === 'vertical' ? 'span' : 'hr');
  el.className = c.direction === 'vertical' ? 'sdk-divider-v' : 'sdk-divider-h';
  if (c.spacing && c.spacing !== 'none') {
    const sp = spacingVar(c.spacing);
    if (c.direction === 'vertical') el.style.margin = `0 ${sp}`;
    else el.style.margin = `${sp} 0`;
  }
  return el;
}

function renderSplit(c, ctx) {
  const el = document.createElement('div');
  el.className = `sdk-split sdk-split-${c.direction === 'vertical' ? 'vertical' : 'horizontal'}`;
  el.style.gap = spacingVar(c.gap || 'md');
  const primary = renderComponent(c.primary, ctx);
  const secondary = renderComponent(c.secondary, ctx);
  if (primary) {
    primary.style.flex = `0 0 ${sizeToCss(c.primary_size).replace('1fr', 'auto')}`;
    el.appendChild(primary);
  }
  if (secondary) {
    secondary.style.flex = '1 1 auto';
    el.appendChild(secondary);
  }
  return el;
}

// =============================================================================
// Container
// =============================================================================

function renderCard(c, ctx) {
  const el = document.createElement('section');
  el.className = 'sdk-card';
  if (c.on_click) {
    el.classList.add('sdk-card-interactive');
    el.addEventListener('click', (ev) => {
      // Nie wyzwalaj on_click karty gdy klik trafil w interaktywne dziecko
      if (ev.target.closest('button, a, input, select, textarea, [role="button"]') !== el) {
        const inner = ev.target.closest('button, a, input, select, textarea, [role="button"]');
        if (inner && el.contains(inner) && inner !== el) return;
      }
      dispatchAction(ctx, null, c.on_click, {});
    });
  }
  if (c.padding) el.style.padding = spacingVar(c.padding);
  if (c.title || c.subtitle || c.icon || (c.actions && c.actions.length)) {
    const head = document.createElement('header');
    head.className = 'sdk-card-header';
    const titleWrap = document.createElement('div');
    if (c.icon) titleWrap.appendChild(renderIcon(c.icon));
    if (c.title) {
      const t = document.createElement('h3');
      t.className = 'sdk-card-title';
      t.textContent = c.title;
      titleWrap.appendChild(t);
    }
    if (c.subtitle) {
      const s = document.createElement('p');
      s.className = 'sdk-card-subtitle';
      s.textContent = c.subtitle;
      titleWrap.appendChild(s);
    }
    head.appendChild(titleWrap);
    if (Array.isArray(c.actions) && c.actions.length) {
      const acts = document.createElement('div');
      acts.className = 'sdk-card-actions';
      for (const a of c.actions) {
        const n = renderComponent(a, ctx);
        if (n) acts.appendChild(n);
      }
      head.appendChild(acts);
    }
    el.appendChild(head);
  }
  const body = document.createElement('div');
  body.className = 'sdk-card-body';
  for (const child of c.children || []) {
    const node = renderComponent(child, ctx);
    if (node) body.appendChild(node);
  }
  el.appendChild(body);
  return el;
}

function renderSectionCard(c, ctx) {
  const el = document.createElement('section');
  el.className = 'sdk-section-card';
  if (c.title || (c.actions && c.actions.length)) {
    const head = document.createElement('header');
    head.className = 'sdk-section-card-header';
    if (c.title) {
      const t = document.createElement('h4');
      t.className = 'sdk-card-title';
      t.textContent = c.title;
      head.appendChild(t);
    }
    if (Array.isArray(c.actions) && c.actions.length) {
      const acts = document.createElement('div');
      acts.className = 'sdk-card-actions';
      for (const a of c.actions) {
        const n = renderComponent(a, ctx);
        if (n) acts.appendChild(n);
      }
      head.appendChild(acts);
    }
    el.appendChild(head);
  }
  for (const child of c.children || []) {
    const node = renderComponent(child, ctx);
    if (node) el.appendChild(node);
  }
  return el;
}

function renderSection(c, ctx) {
  const el = document.createElement('section');
  el.className = 'sdk-section';
  if (c.heading) {
    const h = document.createElement('h2');
    h.className = 'sdk-section-heading';
    h.textContent = c.heading;
    el.appendChild(h);
  }
  for (const child of c.children || []) {
    const node = renderComponent(child, ctx);
    if (node) el.appendChild(node);
  }
  return el;
}

function renderTabs(c, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-tabs';
  const bar = document.createElement('div');
  bar.className = 'sdk-tabs-bar';
  const panes = document.createElement('div');
  panes.className = 'sdk-tabs-panes';
  const activeId = c.active_id || (c.tabs && c.tabs[0] && c.tabs[0].id);
  for (const tab of c.tabs || []) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = `sdk-tab${tab.id === activeId ? ' active' : ''}`;
    if (tab.icon) btn.appendChild(renderIcon(tab.icon));
    btn.appendChild(document.createTextNode(' ' + (tab.label ?? '')));
    btn.addEventListener('click', () => {
      bar.querySelectorAll('.sdk-tab').forEach((t) => t.classList.remove('active'));
      btn.classList.add('active');
      panes.querySelectorAll('[data-tab-pane]').forEach((p) => {
        p.hidden = p.dataset.tabPane !== tab.id;
      });
    });
    bar.appendChild(btn);
    const pane = document.createElement('div');
    pane.className = 'sdk-tab-pane';
    pane.dataset.tabPane = tab.id;
    if (tab.id !== activeId) pane.hidden = true;
    for (const child of tab.children || []) {
      const node = renderComponent(child, ctx);
      if (node) pane.appendChild(node);
    }
    panes.appendChild(pane);
  }
  wrap.appendChild(bar);
  wrap.appendChild(panes);
  return wrap;
}

function renderNavTabs(c, ctx) {
  const el = document.createElement('nav');
  el.className = 'sdk-nav-tabs';
  for (const item of c.items || []) {
    const tab = document.createElement('button');
    tab.type = 'button';
    tab.className = `sdk-nav-tab${item.id === c.active_id ? ' active' : ''}`;
    if (item.icon) tab.appendChild(renderIcon(item.icon));
    tab.appendChild(document.createTextNode(' ' + (item.label ?? '')));
    tab.addEventListener('click', () => navigateToPanel(ctx, item.panel_id));
    el.appendChild(tab);
  }
  return el;
}

function renderToolbar(c, ctx) {
  const el = document.createElement('div');
  el.className = `sdk-toolbar ${c.density || 'normal'}`;
  const left = document.createElement('div');
  left.style.display = 'flex';
  left.style.alignItems = 'center';
  left.style.gap = 'var(--sdk-spacing-sm)';
  if (c.breadcrumb) left.appendChild(renderBreadcrumb(c.breadcrumb, ctx));
  if (c.title) {
    const t = document.createElement('span');
    t.className = 'sdk-toolbar-title';
    t.textContent = c.title;
    left.appendChild(t);
  }
  el.appendChild(left);
  const right = document.createElement('div');
  right.className = 'sdk-toolbar-actions';
  for (const a of c.actions || []) {
    const n = renderComponent(a, ctx);
    if (n) right.appendChild(n);
  }
  el.appendChild(right);
  return el;
}

function renderSidebarContainer(c, ctx) {
  const el = document.createElement('aside');
  el.className = `sdk-sidebar${c.collapsed ? ' collapsed' : ''}`;
  for (const section of c.sections || []) {
    el.appendChild(buildSidebarSection(section, c.active_id, ctx));
  }
  return el;
}

function buildSidebarSection(section, activeId, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-sidebar-section';
  if (section.heading) {
    const h = document.createElement('div');
    h.className = 'sdk-sidebar-heading';
    h.textContent = section.heading;
    wrap.appendChild(h);
  }
  for (const item of section.items || []) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = `sdk-sidebar-item${item.id === activeId ? ' active' : ''}`;
    if (item.icon) btn.appendChild(renderIcon(item.icon));
    const lbl = document.createElement('span');
    lbl.textContent = item.label;
    btn.appendChild(lbl);
    if (item.badge) {
      const b = document.createElement('span');
      b.className = 'sdk-sidebar-badge';
      b.textContent = item.badge;
      btn.appendChild(b);
    }
    if (item.panel_id) {
      btn.addEventListener('click', () => navigateToPanel(ctx, item.panel_id));
    }
    wrap.appendChild(btn);
  }
  return wrap;
}

function renderNavigationSidebar(navigation, ctx) {
  const el = document.createElement('aside');
  el.className = 'sdk-sidebar';
  for (const section of navigation.sidebar || []) {
    el.appendChild(buildSidebarSection(section, navigation.current_panel, ctx));
  }
  return el;
}

function renderCollapsible(c, ctx) {
  const el = document.createElement('div');
  el.className = 'sdk-collapsible';
  if (!c.open) el.setAttribute('hidden-body', '');
  const head = document.createElement('button');
  head.type = 'button';
  head.className = 'sdk-collapsible-header';
  const title = document.createElement('span');
  title.textContent = c.title || '';
  head.appendChild(title);
  const chev = document.createElement('span');
  chev.className = 'sdk-collapsible-chevron';
  chev.textContent = '▼';
  head.appendChild(chev);
  head.addEventListener('click', () => {
    if (el.hasAttribute('hidden-body')) el.removeAttribute('hidden-body');
    else el.setAttribute('hidden-body', '');
  });
  el.appendChild(head);
  const body = document.createElement('div');
  body.className = 'sdk-collapsible-body';
  for (const child of c.children || []) {
    const node = renderComponent(child, ctx);
    if (node) body.appendChild(node);
  }
  el.appendChild(body);
  return el;
}

function renderTooltip(c, ctx) {
  const wrap = document.createElement('span');
  wrap.className = 'sdk-tooltip-wrap';
  wrap.tabIndex = 0;
  const target = renderComponent(c.target, ctx);
  if (target) wrap.appendChild(target);
  const tip = document.createElement('span');
  tip.className = `sdk-tooltip sdk-tooltip-${c.placement || 'top'}`;
  tip.textContent = c.content || '';
  wrap.appendChild(tip);
  return wrap;
}

function renderBreadcrumb(c, ctx) {
  const el = document.createElement('ol');
  el.className = 'sdk-breadcrumb';
  const items = c.items || [];
  items.forEach((item, idx) => {
    const li = document.createElement('li');
    li.className = `sdk-breadcrumb-item${idx === items.length - 1 ? ' current' : ''}`;
    if (item.panel_id && idx !== items.length - 1) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'sdk-breadcrumb-link';
      btn.textContent = item.label;
      btn.addEventListener('click', () => navigateToPanel(ctx, item.panel_id));
      li.appendChild(btn);
    } else {
      li.textContent = item.label;
    }
    el.appendChild(li);
    if (idx < items.length - 1) {
      const sep = document.createElement('span');
      sep.className = 'sdk-breadcrumb-sep';
      sep.textContent = '›';
      el.appendChild(sep);
    }
  });
  return el;
}

function renderPagination(c, ctx) {
  const el = document.createElement('nav');
  el.className = 'sdk-pagination';
  const total = Number(c.total_pages) || 0;
  const current = Number(c.current_page) || 1;
  const siblings = Number(c.sibling_count ?? 1);
  const make = (label, page, opts = {}) => {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = `sdk-pagination-btn${opts.active ? ' active' : ''}`;
    b.textContent = label;
    if (opts.disabled) b.disabled = true;
    if (!opts.disabled && c.on_change) {
      b.addEventListener('click', () => dispatchAction(ctx, null, c.on_change, { page }));
    }
    return b;
  };
  el.appendChild(make('‹', Math.max(1, current - 1), { disabled: current <= 1 }));
  const from = Math.max(1, current - siblings);
  const to = Math.min(total, current + siblings);
  if (from > 1) {
    el.appendChild(make('1', 1));
    if (from > 2) el.appendChild(make('…', null, { disabled: true }));
  }
  for (let p = from; p <= to; p++) el.appendChild(make(String(p), p, { active: p === current }));
  if (to < total) {
    if (to < total - 1) el.appendChild(make('…', null, { disabled: true }));
    el.appendChild(make(String(total), total));
  }
  el.appendChild(make('›', Math.min(total, current + 1), { disabled: current >= total }));
  return el;
}

// =============================================================================
// Overlays — Window / Drawer / Popover renderowane jako portale przy body
// =============================================================================

function createOverlayPortal(overlay, ctx) {
  const c = overlay.content;
  if (!c) return null;
  const portal = document.createElement('div');
  portal.dataset.sdkOverlayPortal = overlay.id || '';
  portal.style.zIndex = String(overlay.z_index || 1000);
  if (c.type === 'window') {
    portal.className = 'sdk-overlay-backdrop';
    portal.style.position = 'fixed';
    portal.style.inset = '0';
    portal.appendChild(buildWindow(c, ctx, overlay));
    if (c.dismissable !== false) {
      portal.addEventListener('click', (e) => {
        if (e.target === portal) closeOverlay(portal, c.on_close, ctx);
      });
    }
  } else if (c.type === 'drawer') {
    portal.className = 'sdk-overlay-backdrop';
    portal.style.position = 'fixed';
    portal.style.inset = '0';
    portal.appendChild(buildDrawer(c, ctx, overlay));
    if (c.dismissable !== false) {
      portal.addEventListener('click', (e) => {
        if (e.target === portal) closeOverlay(portal, c.on_close, ctx);
      });
    }
  } else if (c.type === 'popover') {
    portal.appendChild(buildPopover(c, ctx, overlay));
  } else {
    return null;
  }
  return portal;
}

function buildWindow(c, ctx, overlay) {
  const win = document.createElement('div');
  win.className = `sdk-window sdk-window-${c.size || 'md'}`;
  const head = document.createElement('header');
  head.className = 'sdk-window-header';
  const t = document.createElement('h3');
  t.className = 'sdk-window-title';
  t.textContent = c.title || '';
  head.appendChild(t);
  if (c.dismissable !== false) {
    const close = document.createElement('button');
    close.type = 'button';
    close.className = 'sdk-overlay-close';
    close.textContent = '×';
    close.addEventListener('click', () => closeOverlay(win.closest('[data-sdk-overlay-portal]'), c.on_close, ctx));
    head.appendChild(close);
  }
  win.appendChild(head);
  const body = document.createElement('div');
  body.className = 'sdk-window-body';
  for (const child of c.children || []) {
    const n = renderComponent(child, ctx);
    if (n) body.appendChild(n);
  }
  win.appendChild(body);
  if (Array.isArray(c.footer) && c.footer.length) {
    const foot = document.createElement('footer');
    foot.className = 'sdk-window-footer';
    for (const f of c.footer) {
      const n = renderComponent(f, ctx);
      if (n) foot.appendChild(n);
    }
    win.appendChild(foot);
  }
  return win;
}

function buildDrawer(c, ctx, overlay) {
  const dr = document.createElement('aside');
  dr.className = `sdk-drawer sdk-drawer-${c.side || 'right'}`;
  const head = document.createElement('header');
  head.className = 'sdk-drawer-header';
  const t = document.createElement('h3');
  t.className = 'sdk-drawer-title';
  t.textContent = c.title || '';
  head.appendChild(t);
  if (c.dismissable !== false) {
    const close = document.createElement('button');
    close.type = 'button';
    close.className = 'sdk-overlay-close';
    close.textContent = '×';
    close.addEventListener('click', () => closeOverlay(dr.closest('[data-sdk-overlay-portal]'), c.on_close, ctx));
    head.appendChild(close);
  }
  dr.appendChild(head);
  const body = document.createElement('div');
  body.className = 'sdk-drawer-body';
  for (const child of c.children || []) {
    const n = renderComponent(child, ctx);
    if (n) body.appendChild(n);
  }
  dr.appendChild(body);
  return dr;
}

function buildPopover(c, ctx, overlay) {
  const pop = document.createElement('div');
  pop.className = 'sdk-popover';
  pop.dataset.popoverTarget = c.target_id || '';
  // Bez auto-pozycjonowania w MVP — Faza 3 doda positioning engine.
  for (const child of c.children || []) {
    const n = renderComponent(child, ctx);
    if (n) pop.appendChild(n);
  }
  return pop;
}

function closeOverlay(portalEl, onClose, ctx) {
  if (portalEl) {
    // Animowane zamkniecie — backdrop fade-out, content (jesli wewnatrz) jedzie razem.
    animatedRemove(portalEl, 'leaving', 150);
  }
  if (onClose) dispatchAction(ctx, null, onClose, {});
}

// =============================================================================
// DataDisplay
// =============================================================================

function renderTextV2(c) {
  const tag = c.style && c.style.startsWith('heading') ? 'h3' : 'span';
  const el = document.createElement(tag);
  el.className = `sdk-text sdk-text-${(c.style || 'body').replace(/_/g, '-')}`;
  if (c.truncate) el.classList.add('sdk-text-truncate');
  if (c.align) el.classList.add(`sdk-text-align-${c.align}`);
  if (c.color) el.style.color = `var(--sdk-color-${c.color.replace(/_/g, '-')})`;
  if (c.weight) el.style.fontWeight = `var(--sdk-fw-${c.weight === 'semi_bold' ? 'semibold' : c.weight})`;
  el.textContent = c.content || '';
  return el;
}

function renderHeading(c) {
  const level = (c.level || 'h2').toLowerCase();
  const el = document.createElement(level);
  el.className = `sdk-heading sdk-text-heading${level.replace('h', '')}`;
  if (c.icon) el.appendChild(renderIcon(c.icon));
  el.appendChild(document.createTextNode(c.content || ''));
  if (c.subtitle) {
    const s = document.createElement('span');
    s.className = 'sdk-heading-subtitle';
    s.textContent = c.subtitle;
    el.appendChild(s);
  }
  return el;
}

function renderBadgeV2(c) {
  const el = document.createElement('span');
  el.className = `sdk-badge tone-${c.tone || 'neutral'} ${c.size || 'md'}`;
  el.textContent = c.label || '';
  return el;
}

function renderChipV2(c, ctx) {
  const el = document.createElement('span');
  el.className = `sdk-chip kind-${c.kind || 'category'}`;
  if (c.on_click) {
    el.classList.add('clickable');
    el.addEventListener('click', () => dispatchAction(ctx, null, c.on_click, {}));
  }
  if (c.icon) el.appendChild(renderIcon(c.icon));
  el.appendChild(document.createTextNode(c.label || ''));
  if (c.dismissible && c.on_dismiss) {
    const x = document.createElement('button');
    x.type = 'button';
    x.className = 'sdk-chip-dismiss';
    x.textContent = '×';
    x.addEventListener('click', (e) => { e.stopPropagation(); dispatchAction(ctx, null, c.on_dismiss, {}); });
    el.appendChild(x);
  }
  return el;
}

function renderTag(c) {
  const el = document.createElement('span');
  el.className = 'sdk-tag';
  if (c.color) el.style.color = `var(--sdk-color-${c.color.replace(/_/g, '-')})`;
  el.textContent = c.label || '';
  return el;
}

function renderAvatar(c) {
  const el = document.createElement('span');
  el.className = `sdk-avatar ${c.size || 'md'} ${c.shape || 'circle'}`;
  applyImageSource(el, c.image_source, c.initials);
  if (c.status) {
    const dot = document.createElement('span');
    dot.className = `sdk-avatar-status ${c.status}`;
    el.appendChild(dot);
  }
  return el;
}

function applyImageSource(targetEl, source, fallbackInitials) {
  if (!source) {
    if (fallbackInitials) targetEl.textContent = fallbackInitials;
    return;
  }
  if (source.kind === 'url') {
    const img = document.createElement('img');
    if (isSafeImageSrc(source.url)) img.src = source.url;
    img.alt = '';
    targetEl.appendChild(img);
  } else if (source.kind === 'signed_frame') {
    // Signed-frame zostawiamy na Faza 3 — wymaga API rozwiazania frame_ref.
    targetEl.textContent = '⌧';
    targetEl.title = `signed_frame:${source.camera_id}`;
  } else if (source.kind === 'initials') {
    targetEl.textContent = source.text || '';
    targetEl.style.background = `var(--sdk-color-${(source.background || 'primary').replace(/_/g, '-')})`;
  }
  // placeholder — pusty default
}

function renderImageV2(c) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-image-wrap';
  if (c.radius && c.radius !== 'none') wrap.style.borderRadius = `var(--sdk-radius-${c.radius})`;
  wrap.style.overflow = 'hidden';
  if (c.source?.kind === 'url' && isSafeImageSrc(c.source.url)) {
    const img = document.createElement('img');
    img.className = `sdk-image ${c.fit || 'cover'}`;
    img.src = c.source.url;
    img.alt = c.alt || '';
    if (c.width) img.width = c.width;
    if (c.height) img.height = c.height;
    wrap.appendChild(img);
  } else {
    applyImageSource(wrap, c.source, null);
  }
  return wrap;
}

function renderStat(c) {
  const el = document.createElement('div');
  el.className = 'sdk-stat';
  if (c.icon) {
    const i = renderIcon(c.icon);
    i.style.color = `var(--sdk-color-${(c.accent || 'primary').replace(/_/g, '-')})`;
    el.appendChild(i);
  }
  const v = document.createElement('div');
  v.className = 'sdk-stat-value';
  const rawValue = c.value || '';
  v.textContent = rawValue;
  if (c.accent) v.style.color = `var(--sdk-color-${c.accent.replace(/_/g, '-')})`;
  el.appendChild(v);
  // Animacja licznika 0 -> wartosc — jesli da sie wyparsowac liczbe z tekstu.
  const parsed = parseStatValue(String(rawValue));
  if (parsed && parsed.num !== 0 && !prefersReducedMotion()) {
    v.textContent = parsed.format(0);
    requestAnimationFrame(() => animateNumber(v, 0, parsed.num, 800, parsed.format));
  }
  const l = document.createElement('div');
  l.className = 'sdk-stat-label';
  l.textContent = c.label || '';
  el.appendChild(l);
  if (c.sublabel) {
    const s = document.createElement('div');
    s.className = 'sdk-stat-sublabel';
    s.textContent = c.sublabel;
    el.appendChild(s);
  }
  if (c.trend) {
    const t = document.createElement('div');
    t.className = `sdk-stat-trend ${c.trend.direction || 'neutral'}`;
    const arrow = c.trend.direction === 'up' ? '▲' : c.trend.direction === 'down' ? '▼' : '–';
    t.textContent = `${arrow} ${c.trend.delta}${c.trend.period ? ' / ' + c.trend.period : ''}`;
    el.appendChild(t);
  }
  return el;
}

function renderKeyValue(c, ctx) {
  const el = document.createElement('dl');
  el.className = `sdk-keyvalue ${c.density || 'normal'}`;
  for (const it of c.items || []) {
    const row = document.createElement('div');
    row.className = 'sdk-keyvalue-row';
    const k = document.createElement('dt');
    k.className = 'sdk-keyvalue-key';
    if (it.icon) k.appendChild(renderIcon(it.icon));
    k.appendChild(document.createTextNode(' ' + (it.key ?? '')));
    if (it.tooltip) k.title = it.tooltip;
    const v = document.createElement('dd');
    v.className = 'sdk-keyvalue-value';
    v.appendChild(renderCellValue(it.value, ctx));
    row.appendChild(k);
    row.appendChild(v);
    el.appendChild(row);
  }
  return el;
}

function renderCellValue(cell, ctx) {
  if (!cell || typeof cell !== 'object') return document.createTextNode(String(cell ?? ''));
  switch (cell.cell) {
    case 'text': return document.createTextNode(String(cell.value ?? ''));
    case 'number': {
      const n = Number(cell.value);
      return document.createTextNode(cell.format ? formatNumber(n, cell.format) : String(n));
    }
    case 'boolean': return document.createTextNode(cell.value ? '✓' : '✗');
    case 'date': return document.createTextNode(String(cell.value ?? ''));
    case 'badge': {
      const b = document.createElement('span');
      b.className = `sdk-badge tone-${cell.tone || 'neutral'}`;
      b.textContent = cell.label || '';
      return b;
    }
    case 'chip': {
      const ch = document.createElement('span');
      ch.className = 'sdk-chip';
      if (cell.icon) ch.appendChild(renderIcon(cell.icon));
      ch.appendChild(document.createTextNode(cell.label || ''));
      return ch;
    }
    case 'component': {
      const wrap = document.createElement('span');
      const n = renderComponent(cell.value, ctx);
      if (n) wrap.appendChild(n);
      return wrap;
    }
    case 'empty':
    default: return document.createTextNode('');
  }
}

function formatNumber(n, _fmt) {
  if (!Number.isFinite(n)) return '';
  return String(n);
}

function renderListV2(c, ctx) {
  const el = document.createElement('ul');
  el.className = `sdk-list ${c.density || 'normal'}`;
  for (const it of c.items || []) {
    const li = document.createElement('li');
    li.className = 'sdk-list-item';
    if (it.on_click) {
      li.classList.add('clickable');
      li.addEventListener('click', () => dispatchAction(ctx, null, it.on_click, {}));
    }
    if (it.icon) li.appendChild(renderIcon(it.icon));
    const body = document.createElement('div');
    body.className = 'sdk-list-item-body';
    const title = document.createElement('div');
    title.className = 'sdk-list-item-title';
    title.textContent = it.title || '';
    body.appendChild(title);
    if (it.subtitle) {
      const s = document.createElement('div');
      s.className = 'sdk-list-item-subtitle';
      s.textContent = it.subtitle;
      body.appendChild(s);
    }
    li.appendChild(body);
    if (it.trailing) {
      const tr = document.createElement('span');
      tr.className = 'sdk-list-item-trailing';
      tr.appendChild(renderCellValue(it.trailing, ctx));
      li.appendChild(tr);
    }
    el.appendChild(li);
  }
  requestAnimationFrame(() => staggerEnter(el, ':scope > .sdk-list-item', 30, 250));
  return el;
}

function renderBulletList(c) {
  const el = document.createElement('ul');
  el.className = `sdk-bullet-list ${c.style || 'disc'}`;
  for (const item of c.items || []) {
    const li = document.createElement('li');
    li.textContent = String(item);
    el.appendChild(li);
  }
  return el;
}

function renderTimeline(c) {
  const el = document.createElement('ol');
  el.className = `sdk-timeline ${c.orientation === 'horizontal' ? 'horizontal' : ''}`;
  for (const it of c.items || []) {
    const li = document.createElement('li');
    li.className = 'sdk-timeline-item';
    if (it.accent) li.style.setProperty('--sdk-color-primary', `var(--sdk-color-${it.accent.replace(/_/g, '-')})`);
    const t = document.createElement('div');
    t.className = 'sdk-timeline-time';
    t.textContent = it.timestamp || '';
    const ti = document.createElement('div');
    ti.className = 'sdk-timeline-title';
    if (it.icon) ti.appendChild(renderIcon(it.icon));
    ti.appendChild(document.createTextNode(' ' + (it.title || '')));
    li.appendChild(t);
    li.appendChild(ti);
    if (it.description) {
      const d = document.createElement('div');
      d.className = 'sdk-timeline-desc';
      d.textContent = it.description;
      li.appendChild(d);
    }
    el.appendChild(li);
  }
  requestAnimationFrame(() => staggerEnter(el, ':scope > .sdk-timeline-item', 35, 250));
  return el;
}

function renderTableV2(c, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-table-wrap';
  const t = document.createElement('table');
  t.className = `sdk-table ${c.density || 'normal'}`;
  const thead = document.createElement('thead');
  const trH = document.createElement('tr');
  for (const col of c.columns || []) {
    const th = document.createElement('th');
    th.textContent = col.label || '';
    th.style.textAlign = col.align === 'center' ? 'center' : col.align === 'end' ? 'right' : 'left';
    if (col.width) th.style.width = `${col.width}px`;
    if (col.sortable && c.on_sort) {
      th.classList.add('sdk-table-sortable');
      th.addEventListener('click', () => dispatchAction(ctx, null, c.on_sort, { columnId: col.id }));
    }
    if (col.icon) {
      const ic = renderIcon(col.icon);
      th.prepend(ic);
    }
    trH.appendChild(th);
  }
  thead.appendChild(trH);
  t.appendChild(thead);
  const tbody = document.createElement('tbody');
  const rows = c.rows || [];
  if (rows.length === 0) {
    const tr = document.createElement('tr');
    const td = document.createElement('td');
    td.colSpan = (c.columns || []).length || 1;
    td.className = 'sdk-table-empty';
    td.textContent = c.empty_state || 'Brak danych';
    tr.appendChild(td);
    tbody.appendChild(tr);
  } else {
    for (const row of rows) {
      const tr = document.createElement('tr');
      tr.dataset.rowId = row.id;
      if (row.accent) tr.style.borderLeft = `3px solid var(--sdk-color-${row.accent.replace(/_/g, '-')})`;
      if (c.on_row_click) {
        tr.classList.add('clickable');
        tr.addEventListener('click', () => dispatchAction(ctx, null, c.on_row_click, { rowId: row.id }));
      }
      for (const cell of row.cells || []) {
        const td = document.createElement('td');
        td.appendChild(renderCellValue(cell, ctx));
        tr.appendChild(td);
      }
      tbody.appendChild(tr);
      if (c.expandable && Array.isArray(row.expanded_content) && row.expanded_content.length) {
        const trEx = document.createElement('tr');
        const tdEx = document.createElement('td');
        tdEx.colSpan = (c.columns || []).length || 1;
        for (const ec of row.expanded_content) {
          const n = renderComponent(ec, ctx);
          if (n) tdEx.appendChild(n);
        }
        trEx.appendChild(tdEx);
        tbody.appendChild(trEx);
      }
    }
  }
  t.appendChild(tbody);
  wrap.appendChild(t);
  if (rows.length > 0 && rows.length <= 30) {
    // Stagger enter tylko dla rozsądnie krótkich tabel — przy 200 wierszach
    // delay sumarycznie wybiega poza 60fps budget i wygląda na lag.
    requestAnimationFrame(() => staggerEnter(tbody, ':scope > tr', 20, 250));
  }
  if (c.pagination) {
    const pg = document.createElement('div');
    pg.className = 'sdk-table-pagination';
    if (c.pagination.kind === 'pages') {
      pg.appendChild(renderPagination({
        type: 'pagination',
        current_page: c.pagination.current_page,
        total_pages: c.pagination.total_pages,
        on_change: c.pagination.on_page_change,
      }, ctx));
    } else if (c.pagination.kind === 'cursor' && c.on_load_more) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'sdk-button variant-secondary';
      btn.textContent = 'Załaduj więcej';
      if (!c.pagination.next_cursor) btn.disabled = true;
      btn.addEventListener('click', () => dispatchAction(ctx, null, c.on_load_more, { cursor: c.pagination.next_cursor || null }));
      pg.appendChild(btn);
    }
    wrap.appendChild(pg);
  }
  return wrap;
}

function renderMonoBlock(c) {
  const pre = document.createElement('pre');
  pre.className = 'sdk-mono-block';
  pre.textContent = c.content || '';
  if (c.language) pre.dataset.language = c.language;
  return pre;
}

function renderCodeBlock(c) {
  const pre = document.createElement('pre');
  pre.className = 'sdk-code-block';
  if (c.language) pre.dataset.language = c.language;
  const segs = Array.isArray(c.segments) ? c.segments : [];
  if (c.show_line_numbers) {
    const lines = [];
    let lineBuf = [];
    for (const seg of segs) {
      const parts = String(seg.text ?? '').split('\n');
      parts.forEach((part, idx) => {
        const span = document.createElement('span');
        span.className = `seg-${seg.kind || 'plain'}`;
        span.textContent = part;
        lineBuf.push(span);
        if (idx < parts.length - 1) {
          lines.push(lineBuf);
          lineBuf = [];
        }
      });
    }
    if (lineBuf.length) lines.push(lineBuf);
    lines.forEach((line, idx) => {
      const num = document.createElement('span');
      num.className = 'sdk-code-line-num';
      num.textContent = String(idx + 1).padStart(3, ' ');
      pre.appendChild(num);
      for (const s of line) pre.appendChild(s);
      pre.appendChild(document.createTextNode('\n'));
    });
  } else {
    for (const seg of segs) {
      const span = document.createElement('span');
      span.className = `seg-${seg.kind || 'plain'}`;
      span.textContent = String(seg.text ?? '');
      pre.appendChild(span);
    }
  }
  return pre;
}

function renderEmptyState(c, ctx) {
  const el = document.createElement('div');
  el.className = 'sdk-empty-state';
  if (c.icon) el.appendChild(renderIcon(c.icon));
  if (c.title) {
    const h = document.createElement('h3');
    h.className = 'sdk-empty-state-title';
    h.textContent = c.title;
    el.appendChild(h);
  }
  if (c.message) {
    const p = document.createElement('p');
    p.className = 'sdk-empty-state-message';
    p.textContent = c.message;
    el.appendChild(p);
  }
  if (Array.isArray(c.actions) && c.actions.length) {
    const a = document.createElement('div');
    a.className = 'sdk-empty-state-actions';
    for (const ac of c.actions) {
      const n = renderComponent(ac, ctx);
      if (n) a.appendChild(n);
    }
    el.appendChild(a);
  }
  return el;
}

// =============================================================================
// Form
// =============================================================================

function buildFieldShell(c, controlEl, ctx) {
  const wrap = document.createElement('label');
  wrap.className = 'sdk-field';
  if (c.label) {
    const lbl = document.createElement('span');
    lbl.className = 'sdk-field-label';
    lbl.textContent = c.label;
    if (c.required) {
      const req = document.createElement('span');
      req.className = 'sdk-field-required';
      req.textContent = '*';
      lbl.appendChild(req);
    }
    wrap.appendChild(lbl);
  }
  wrap.appendChild(controlEl);
  if (c.helper) {
    const h = document.createElement('span');
    h.className = 'sdk-field-helper';
    h.textContent = c.helper;
    wrap.appendChild(h);
  }
  return wrap;
}

function attachChange(el, handler, ctx, params) {
  if (!handler) return;
  el.addEventListener('change', (ev) => {
    const value = 'value' in ev.target ? ev.target.value : ev.target.checked;
    dispatchAction(ctx, null, handler, { ...(params || {}), value });
  });
}

function renderInputV2(c, ctx) {
  const wrapIcon = document.createElement('div');
  wrapIcon.className = 'sdk-input-icon-wrap';
  const input = document.createElement('input');
  input.type = mapInputKind(c.kind);
  input.className = 'sdk-input';
  input.name = c.id || '';
  input.dataset.fieldId = c.id || '';
  if (c.value != null) input.value = String(c.value);
  if (c.placeholder) input.placeholder = c.placeholder;
  if (c.disabled) input.disabled = true;
  if (c.readonly) input.readOnly = true;
  if (c.required) input.required = true;
  if (c.autocomplete) input.autocomplete = c.autocomplete;
  if (c.icon) {
    const ic = renderIcon(c.icon);
    ic.className = 'sdk-input-icon';
    wrapIcon.appendChild(ic);
  }
  wrapIcon.appendChild(input);
  if (c.suffix) {
    const sfx = document.createElement('span');
    sfx.className = 'sdk-input-suffix';
    sfx.textContent = c.suffix;
    wrapIcon.appendChild(sfx);
  }
  attachChange(input, c.on_change, ctx, { id: c.id });
  if (c.on_submit) {
    input.addEventListener('keydown', (ev) => {
      if (ev.key === 'Enter') {
        ev.preventDefault();
        dispatchAction(ctx, null, c.on_submit, { id: c.id, value: input.value });
      }
    });
  }
  return buildFieldShell(c, wrapIcon, ctx);
}

function mapInputKind(k) {
  switch (k) {
    case 'email': return 'email';
    case 'password': return 'password';
    case 'number': return 'number';
    case 'url': return 'url';
    case 'tel': return 'tel';
    case 'search': return 'search';
    case 'text':
    default: return 'text';
  }
}

function renderTextarea(c, ctx) {
  const ta = document.createElement('textarea');
  ta.className = 'sdk-textarea';
  ta.name = c.id || '';
  ta.dataset.fieldId = c.id || '';
  if (c.value != null) ta.value = String(c.value);
  if (c.placeholder) ta.placeholder = c.placeholder;
  if (c.rows) ta.rows = c.rows;
  if (c.disabled) ta.disabled = true;
  if (c.readonly) ta.readOnly = true;
  if (c.required) ta.required = true;
  attachChange(ta, c.on_change, ctx, { id: c.id });
  return buildFieldShell(c, ta, ctx);
}

function renderSelectV2(c, ctx) {
  const sel = document.createElement('select');
  sel.className = 'sdk-select';
  sel.name = c.id || '';
  sel.dataset.fieldId = c.id || '';
  if (c.disabled) sel.disabled = true;
  if (c.required) sel.required = true;
  if (c.placeholder) {
    const opt = document.createElement('option');
    opt.value = '';
    opt.textContent = c.placeholder;
    opt.disabled = true;
    opt.selected = c.value == null;
    sel.appendChild(opt);
  }
  // Grupowanie po `group` z optgroup gdy obecne
  let currentGroup = null;
  let optgroup = null;
  for (const opt of c.options || []) {
    if (opt.group && opt.group !== currentGroup) {
      currentGroup = opt.group;
      optgroup = document.createElement('optgroup');
      optgroup.label = opt.group;
      sel.appendChild(optgroup);
    }
    const o = document.createElement('option');
    o.value = opt.value;
    o.textContent = opt.label;
    if (opt.disabled) o.disabled = true;
    if (c.value === opt.value) o.selected = true;
    (optgroup || sel).appendChild(o);
  }
  attachChange(sel, c.on_change, ctx, { id: c.id });
  return buildFieldShell(c, sel, ctx);
}

function renderMultiSelect(c, ctx) {
  const sel = document.createElement('select');
  sel.className = 'sdk-select';
  sel.name = c.id || '';
  sel.dataset.fieldId = c.id || '';
  sel.multiple = true;
  const values = new Set((c.values || []).map(String));
  for (const opt of c.options || []) {
    const o = document.createElement('option');
    o.value = opt.value;
    o.textContent = opt.label;
    if (opt.disabled) o.disabled = true;
    if (values.has(opt.value)) o.selected = true;
    sel.appendChild(o);
  }
  if (c.disabled) sel.disabled = true;
  if (c.on_change) {
    sel.addEventListener('change', () => {
      const selected = Array.from(sel.selectedOptions).map((o) => o.value);
      dispatchAction(ctx, null, c.on_change, { id: c.id, values: selected });
    });
  }
  return buildFieldShell(c, sel, ctx);
}

function renderCheckbox(c, ctx) {
  const wrap = document.createElement('label');
  wrap.className = 'sdk-checkbox';
  const cb = document.createElement('input');
  cb.type = 'checkbox';
  cb.name = c.id || '';
  cb.dataset.fieldId = c.id || '';
  cb.checked = Boolean(c.value);
  if (c.disabled) cb.disabled = true;
  if (c.on_change) {
    cb.addEventListener('change', () => dispatchAction(ctx, null, c.on_change, { id: c.id, value: cb.checked }));
  }
  wrap.appendChild(cb);
  wrap.appendChild(document.createTextNode(' ' + (c.label || '')));
  if (c.helper) {
    const h = document.createElement('div');
    h.className = 'sdk-field-helper';
    h.textContent = c.helper;
    wrap.appendChild(h);
  }
  return wrap;
}

function renderRadio(c, ctx) {
  const wrap = document.createElement('label');
  wrap.className = 'sdk-radio-line';
  const r = document.createElement('input');
  r.type = 'radio';
  r.name = c.id || '';
  r.checked = Boolean(c.value);
  if (c.disabled) r.disabled = true;
  if (c.on_change) {
    r.addEventListener('change', () => dispatchAction(ctx, null, c.on_change, { id: c.id, value: r.checked }));
  }
  wrap.appendChild(r);
  wrap.appendChild(document.createTextNode(' ' + (c.label || '')));
  return wrap;
}

function renderRadioGroup(c, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-field';
  if (c.label) {
    const l = document.createElement('span');
    l.className = 'sdk-field-label';
    l.textContent = c.label;
    wrap.appendChild(l);
  }
  const group = document.createElement('div');
  group.className = `sdk-radio-group ${c.orientation || 'vertical'}`;
  for (const opt of c.options || []) {
    const lbl = document.createElement('label');
    lbl.className = 'sdk-radio-line';
    const r = document.createElement('input');
    r.type = 'radio';
    r.name = c.id || '';
    r.value = opt.value;
    r.checked = c.value === opt.value;
    if (opt.disabled || c.disabled) r.disabled = true;
    if (c.on_change) {
      r.addEventListener('change', () => dispatchAction(ctx, null, c.on_change, { id: c.id, value: opt.value }));
    }
    lbl.appendChild(r);
    lbl.appendChild(document.createTextNode(' ' + (opt.label || '')));
    group.appendChild(lbl);
  }
  wrap.appendChild(group);
  if (c.helper) {
    const h = document.createElement('span');
    h.className = 'sdk-field-helper';
    h.textContent = c.helper;
    wrap.appendChild(h);
  }
  return wrap;
}

function renderRadioCardGroup(c, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-field';
  if (c.label) {
    const l = document.createElement('span');
    l.className = 'sdk-field-label';
    l.textContent = c.label;
    wrap.appendChild(l);
  }
  const group = document.createElement('div');
  group.className = 'sdk-radio-card-group';
  group.style.gridTemplateColumns = `repeat(${c.columns || 2}, 1fr)`;
  for (const opt of c.options || []) {
    const card = document.createElement('button');
    card.type = 'button';
    card.className = `sdk-radio-card${c.value === opt.value ? ' selected' : ''}`;
    if (opt.disabled) card.disabled = true;
    if (opt.icon) card.appendChild(renderIcon(opt.icon));
    const body = document.createElement('div');
    const t = document.createElement('div');
    t.className = 'sdk-radio-card-title';
    t.textContent = opt.title || '';
    body.appendChild(t);
    if (opt.description) {
      const d = document.createElement('div');
      d.className = 'sdk-radio-card-desc';
      d.textContent = opt.description;
      body.appendChild(d);
    }
    if (opt.badge) {
      const b = document.createElement('span');
      b.className = 'sdk-badge tone-info';
      b.textContent = opt.badge;
      body.appendChild(b);
    }
    card.appendChild(body);
    if (c.on_change) {
      card.addEventListener('click', () => dispatchAction(ctx, null, c.on_change, { id: c.id, value: opt.value }));
    }
    group.appendChild(card);
  }
  wrap.appendChild(group);
  return wrap;
}

function renderToggleV2(c, ctx) {
  const wrap = document.createElement('button');
  wrap.type = 'button';
  wrap.className = `sdk-toggle ${c.size || 'md'}${c.value ? ' on' : ''}`;
  if (c.disabled) wrap.disabled = true;
  const sw = document.createElement('span');
  sw.className = 'sdk-toggle-switch';
  wrap.appendChild(sw);
  wrap.appendChild(document.createTextNode(' ' + (c.label || '')));
  if (c.on_change) {
    wrap.addEventListener('click', () => {
      const newVal = !wrap.classList.contains('on');
      wrap.classList.toggle('on', newVal);
      dispatchAction(ctx, null, c.on_change, { id: c.id, value: newVal });
    });
  }
  return wrap;
}

function renderSlider(c, ctx) {
  const slider = document.createElement('input');
  slider.type = 'range';
  slider.className = 'sdk-slider';
  slider.name = c.id || '';
  slider.dataset.fieldId = c.id || '';
  slider.min = String(c.min);
  slider.max = String(c.max);
  if (c.step != null) slider.step = String(c.step);
  if (c.value != null) slider.value = String(c.value);
  if (c.disabled) slider.disabled = true;
  if (c.on_change) {
    slider.addEventListener('change', () => dispatchAction(ctx, null, c.on_change, { id: c.id, value: Number(slider.value) }));
  }
  return buildFieldShell(c, slider, ctx);
}

function renderSliderRow(c, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-slider-row';
  const lbl = document.createElement('span');
  lbl.className = 'sdk-field-label';
  lbl.textContent = c.label || '';
  wrap.appendChild(lbl);
  const slider = document.createElement('input');
  slider.type = 'range';
  slider.className = 'sdk-slider';
  slider.min = String(c.min);
  slider.max = String(c.max);
  if (c.step != null) slider.step = String(c.step);
  slider.value = String(c.value);
  if (c.accent) slider.style.accentColor = `var(--sdk-color-${c.accent.replace(/_/g, '-')})`;
  wrap.appendChild(slider);
  const val = document.createElement('span');
  val.className = 'sdk-slider-row-value';
  val.textContent = formatSliderValue(c.value, c.value_format);
  wrap.appendChild(val);
  slider.addEventListener('input', () => { val.textContent = formatSliderValue(Number(slider.value), c.value_format); });
  if (c.on_change) {
    slider.addEventListener('change', () => dispatchAction(ctx, null, c.on_change, { id: c.id, value: Number(slider.value) }));
  }
  return wrap;
}

function formatSliderValue(v, fmt) {
  if (!fmt) return String(v);
  return String(v) + (fmt.startsWith('%') ? '' : fmt);
}

function renderDatePicker(c, ctx) {
  const i = document.createElement('input');
  i.type = 'date';
  i.className = 'sdk-input';
  i.name = c.id || '';
  i.dataset.fieldId = c.id || '';
  if (c.value) i.value = c.value;
  if (c.min) i.min = c.min;
  if (c.max) i.max = c.max;
  if (c.disabled) i.disabled = true;
  if (c.required) i.required = true;
  attachChange(i, c.on_change, ctx, { id: c.id });
  return buildFieldShell(c, i, ctx);
}

function renderDateRangePicker(c, ctx) {
  const wrap = document.createElement('div');
  wrap.style.display = 'flex';
  wrap.style.gap = 'var(--sdk-spacing-sm)';
  const from = document.createElement('input');
  from.type = 'date';
  from.className = 'sdk-input';
  from.dataset.fieldId = `${c.id}_from`;
  if (c.from) from.value = c.from;
  if (c.min) from.min = c.min;
  if (c.max) from.max = c.max;
  const to = document.createElement('input');
  to.type = 'date';
  to.className = 'sdk-input';
  to.dataset.fieldId = `${c.id}_to`;
  if (c.to) to.value = c.to;
  if (c.min) to.min = c.min;
  if (c.max) to.max = c.max;
  if (c.disabled) { from.disabled = true; to.disabled = true; }
  wrap.appendChild(from);
  wrap.appendChild(to);
  const emit = () => {
    if (!c.on_change) return;
    dispatchAction(ctx, null, c.on_change, { id: c.id, from: from.value, to: to.value });
  };
  from.addEventListener('change', emit);
  to.addEventListener('change', emit);
  if (Array.isArray(c.presets) && c.presets.length) {
    const pr = document.createElement('div');
    pr.className = 'sdk-button-group';
    for (const p of c.presets) {
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'sdk-button sm variant-ghost';
      b.textContent = p.label;
      b.addEventListener('click', () => {
        from.value = p.from; to.value = p.to;
        if (c.on_change) dispatchAction(ctx, null, c.on_change, { id: c.id, from: p.from, to: p.to });
      });
      pr.appendChild(b);
    }
    wrap.appendChild(pr);
  }
  return buildFieldShell(c, wrap, ctx);
}

function renderTimePicker(c, ctx) {
  const i = document.createElement('input');
  i.type = 'time';
  i.className = 'sdk-input';
  i.name = c.id || '';
  i.dataset.fieldId = c.id || '';
  if (c.value) i.value = c.value;
  if (c.disabled) i.disabled = true;
  if (c.required) i.required = true;
  attachChange(i, c.on_change, ctx, { id: c.id });
  return buildFieldShell(c, i, ctx);
}

function renderFileUpload(c, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-file-upload';
  const i = document.createElement('input');
  i.type = 'file';
  if (Array.isArray(c.accept) && c.accept.length) i.accept = c.accept.join(',');
  if (c.multiple) i.multiple = true;
  i.dataset.fieldId = c.id || '';
  if (c.on_change) {
    i.addEventListener('change', () => {
      const files = Array.from(i.files || []).map((f) => ({ name: f.name, size: f.size, type: f.type }));
      dispatchAction(ctx, null, c.on_change, { id: c.id, files });
    });
  }
  wrap.appendChild(i);
  if (Array.isArray(c.files) && c.files.length) {
    const list = document.createElement('div');
    list.className = 'sdk-file-list';
    for (const f of c.files) {
      const row = document.createElement('div');
      row.className = `sdk-file-row${f.error ? ' error' : ''}`;
      const name = document.createElement('span');
      name.textContent = `${f.name} (${formatBytes(f.size_bytes)})`;
      row.appendChild(name);
      if (c.on_remove) {
        const rm = document.createElement('button');
        rm.type = 'button';
        rm.className = 'sdk-button sm variant-ghost';
        rm.textContent = 'Usuń';
        rm.addEventListener('click', () => dispatchAction(ctx, null, c.on_remove, { id: c.id, fileId: f.id }));
        row.appendChild(rm);
      }
      list.appendChild(row);
    }
    wrap.appendChild(list);
  }
  return buildFieldShell(c, wrap, ctx);
}

function formatBytes(n) {
  if (!Number.isFinite(n)) return '';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function renderSearchV2(c, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-search';
  wrap.appendChild(renderIcon('search'));
  const i = document.createElement('input');
  i.type = 'search';
  if (c.placeholder) i.placeholder = c.placeholder;
  if (c.value != null) i.value = String(c.value);
  i.dataset.fieldId = c.id || '';
  if (c.autofocus) queueMicrotask(() => i.focus());
  wrap.appendChild(i);
  let debounceTimer = null;
  const debounceMs = Number(c.debounce_ms) || 0;
  if (c.on_change) {
    i.addEventListener('input', () => {
      if (debounceTimer) clearTimeout(debounceTimer);
      const v = i.value;
      const fire = () => dispatchAction(ctx, null, c.on_change, { id: c.id, value: v });
      if (debounceMs > 0) debounceTimer = setTimeout(fire, debounceMs); else fire();
    });
  }
  if (c.on_submit) {
    i.addEventListener('keydown', (ev) => {
      if (ev.key === 'Enter') {
        ev.preventDefault();
        dispatchAction(ctx, null, c.on_submit, { id: c.id, value: i.value });
      }
    });
  }
  return wrap;
}

function renderFormV2(c, ctx) {
  const form = document.createElement('form');
  form.className = c.layout === 'grid' ? 'sdk-form sdk-form-grid' : 'sdk-form';
  form.dataset.formId = c.id || '';
  if (c.disabled) form.setAttribute('aria-disabled', 'true');
  for (const child of c.children || []) {
    const n = renderComponent(child, ctx);
    if (n) form.appendChild(n);
  }
  const footer = document.createElement('div');
  footer.className = 'sdk-form-footer';
  if (c.on_cancel) {
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'sdk-button variant-ghost';
    cancel.textContent = c.cancel_label || 'Anuluj';
    cancel.addEventListener('click', () => dispatchAction(ctx, null, c.on_cancel, {}));
    footer.appendChild(cancel);
  }
  const submit = document.createElement('button');
  submit.type = 'submit';
  submit.className = 'sdk-button';
  submit.textContent = c.submit_label || 'Wyślij';
  if (c.disabled) submit.disabled = true;
  footer.appendChild(submit);
  form.appendChild(footer);
  form.addEventListener('submit', (ev) => {
    ev.preventDefault();
    if (!c.on_submit) return;
    const values = collectFormValues(form);
    dispatchAction(ctx, null, c.on_submit, { id: c.id, values });
  });
  return form;
}

function renderFormField(c, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-field';
  if (c.label) {
    const l = document.createElement('span');
    l.className = 'sdk-field-label';
    l.textContent = c.label;
    if (c.required) {
      const r = document.createElement('span');
      r.className = 'sdk-field-required';
      r.textContent = '*';
      l.appendChild(r);
    }
    wrap.appendChild(l);
  }
  const child = renderComponent(c.child, ctx);
  if (child) {
    if (c.field_id && child instanceof HTMLElement && !child.dataset.fieldId) child.dataset.fieldId = c.field_id;
    wrap.appendChild(child);
  }
  if (c.helper) {
    const h = document.createElement('span');
    h.className = 'sdk-field-helper';
    h.textContent = c.helper;
    wrap.appendChild(h);
  }
  return wrap;
}

function renderFormGroup(c, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-form-group';
  if (c.heading) {
    const h = document.createElement('h4');
    h.className = 'sdk-form-group-heading';
    h.textContent = c.heading;
    wrap.appendChild(h);
  }
  if (c.description) {
    const d = document.createElement('p');
    d.className = 'sdk-form-group-desc';
    d.textContent = c.description;
    wrap.appendChild(d);
  }
  for (const child of c.children || []) {
    const n = renderComponent(child, ctx);
    if (n) wrap.appendChild(n);
  }
  return wrap;
}

// =============================================================================
// Feedback
// =============================================================================

function renderAlert(c, ctx) {
  const el = document.createElement('div');
  el.className = `sdk-alert tone-${c.tone || 'info'}`;
  if (c.icon) el.appendChild(renderIcon(c.icon));
  const body = document.createElement('div');
  body.className = 'sdk-alert-body';
  if (c.title) {
    const t = document.createElement('div');
    t.className = 'sdk-alert-title';
    t.textContent = c.title;
    body.appendChild(t);
  }
  const m = document.createElement('div');
  m.className = 'sdk-alert-message';
  m.textContent = c.message || '';
  body.appendChild(m);
  if (Array.isArray(c.actions) && c.actions.length) {
    const a = document.createElement('div');
    a.className = 'sdk-alert-actions';
    for (const ac of c.actions) {
      const n = renderComponent(ac, ctx);
      if (n) a.appendChild(n);
    }
    body.appendChild(a);
  }
  el.appendChild(body);
  if (c.dismissible) {
    const x = document.createElement('button');
    x.type = 'button';
    x.className = 'sdk-alert-dismiss';
    x.textContent = '×';
    x.addEventListener('click', () => {
      el.remove();
      if (c.on_dismiss) dispatchAction(ctx, null, c.on_dismiss, {});
    });
    el.appendChild(x);
  }
  return el;
}

function renderBanner(c, ctx) {
  const el = document.createElement('div');
  el.className = `sdk-banner tone-${c.tone || 'info'}`;
  if (c.icon) el.appendChild(renderIcon(c.icon));
  const body = document.createElement('div');
  body.className = 'sdk-banner-body';
  body.textContent = c.message || '';
  el.appendChild(body);
  if (c.action_label && c.on_action) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'sdk-button sm variant-secondary';
    b.textContent = c.action_label;
    b.addEventListener('click', () => dispatchAction(ctx, null, c.on_action, {}));
    el.appendChild(b);
  }
  if (c.dismissible) {
    const x = document.createElement('button');
    x.type = 'button';
    x.className = 'sdk-alert-dismiss';
    x.textContent = '×';
    x.addEventListener('click', () => {
      el.remove();
      if (c.on_dismiss) dispatchAction(ctx, null, c.on_dismiss, {});
    });
    el.appendChild(x);
  }
  return el;
}

function renderCallout(c, ctx) {
  const el = document.createElement('div');
  el.className = `sdk-callout tone-${c.tone || 'info'}`;
  if (c.icon) el.appendChild(renderIcon(c.icon));
  const body = document.createElement('div');
  body.className = 'sdk-callout-body';
  if (c.title) {
    const t = document.createElement('div');
    t.className = 'sdk-callout-title';
    t.textContent = c.title;
    body.appendChild(t);
  }
  for (const child of c.children || []) {
    const n = renderComponent(child, ctx);
    if (n) body.appendChild(n);
  }
  el.appendChild(body);
  return el;
}

function renderToastInline(c, ctx) {
  // Toast jako element seed'owany w panelu (deklaratywny). Runtime toasts
  // beda emitowane przez host fn `ui_toast` poza tym rendererem.
  const el = document.createElement('div');
  el.className = `sdk-toast tone-${c.tone || 'info'}`;
  el.dataset.toastId = c.id || '';
  if (c.icon) el.appendChild(renderIcon(c.icon));
  const body = document.createElement('div');
  if (c.title) {
    const t = document.createElement('strong');
    t.textContent = c.title;
    body.appendChild(t);
    body.appendChild(document.createElement('br'));
  }
  body.appendChild(document.createTextNode(c.message || ''));
  el.appendChild(body);
  if (c.action_label && c.on_action) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'sdk-button sm variant-ghost';
    b.textContent = c.action_label;
    b.addEventListener('click', () => dispatchAction(ctx, null, c.on_action, {}));
    el.appendChild(b);
  }
  return el;
}

function renderSpinner(c) {
  const wrap = document.createElement('span');
  wrap.style.display = 'inline-flex';
  wrap.style.alignItems = 'center';
  const s = document.createElement('span');
  s.className = `sdk-spinner ${c.size || 'md'}`;
  wrap.appendChild(s);
  if (c.label) {
    const l = document.createElement('span');
    l.className = 'sdk-spinner-label';
    l.textContent = c.label;
    wrap.appendChild(l);
  }
  return wrap;
}

function renderProgressV2(c) {
  const wrap = document.createElement('div');
  wrap.className = `sdk-progress ${c.size || 'md'} tone-${c.tone || 'info'}${c.indeterminate ? ' indeterminate' : ''}`;
  const bar = document.createElement('div');
  bar.className = 'sdk-progress-bar';
  const fill = document.createElement('div');
  fill.className = 'sdk-progress-fill';
  const max = Number(c.max) || 100;
  const val = Number(c.value) || 0;
  const pct = c.indeterminate ? 30 : Math.min(100, Math.max(0, (val / max) * 100));
  fill.style.width = `${pct}%`;
  bar.appendChild(fill);
  wrap.appendChild(bar);
  if (c.label || c.show_percent) {
    const lbl = document.createElement('div');
    lbl.className = 'sdk-progress-label';
    const left = document.createElement('span');
    left.textContent = c.label || '';
    const right = document.createElement('span');
    if (c.show_percent && !c.indeterminate) right.textContent = `${Math.round(pct)}%`;
    lbl.appendChild(left);
    lbl.appendChild(right);
    wrap.appendChild(lbl);
  }
  return wrap;
}

function renderSkeleton(c) {
  const wrap = document.createElement('div');
  const lines = Math.max(1, Number(c.lines) || 3);
  const width = Number(c.width_percent) || 100;
  for (let i = 0; i < lines; i++) {
    const el = document.createElement('div');
    el.className = `sdk-skeleton ${c.shape || 'block'}`;
    el.style.width = `${i === lines - 1 ? Math.max(40, width - 30) : width}%`;
    wrap.appendChild(el);
  }
  return wrap;
}

function renderHint(c) {
  const el = document.createElement('span');
  el.className = `sdk-hint tone-${c.tone || 'info'}`;
  if (c.icon) el.appendChild(renderIcon(c.icon));
  el.appendChild(document.createTextNode(' ' + (c.message || '')));
  return el;
}

function renderGateScreen(c, ctx) {
  const el = document.createElement('div');
  el.className = 'sdk-gate-screen';
  if (c.icon) el.appendChild(renderIcon(c.icon));
  const t = document.createElement('h2');
  t.className = 'sdk-gate-title';
  t.textContent = c.title || '';
  el.appendChild(t);
  const m = document.createElement('p');
  m.className = 'sdk-gate-message';
  m.textContent = c.message || '';
  el.appendChild(m);
  if (Array.isArray(c.requirements) && c.requirements.length) {
    const ul = document.createElement('ul');
    ul.className = 'sdk-gate-reqs';
    for (const r of c.requirements) {
      const li = document.createElement('li');
      li.className = `sdk-gate-req ${r.satisfied ? 'satisfied' : 'unsatisfied'}`;
      li.textContent = `${r.satisfied ? '✓' : '✗'} ${r.label}`;
      if (r.description) {
        const d = document.createElement('span');
        d.style.color = 'var(--sdk-color-text-subtle)';
        d.style.marginLeft = '8px';
        d.textContent = r.description;
        li.appendChild(d);
      }
      ul.appendChild(li);
    }
    el.appendChild(ul);
  }
  if (Array.isArray(c.actions) && c.actions.length) {
    const a = document.createElement('div');
    a.style.display = 'flex';
    a.style.gap = 'var(--sdk-spacing-sm)';
    for (const ac of c.actions) {
      const n = renderComponent(ac, ctx);
      if (n) a.appendChild(n);
    }
    el.appendChild(a);
  }
  return el;
}

// =============================================================================
// Action
// =============================================================================

function renderButtonV2(c, ctx) {
  const el = document.createElement('button');
  el.type = 'button';
  el.className = `sdk-button variant-${c.variant || 'primary'} ${c.size || 'md'}${c.full_width ? ' full-width' : ''}${c.loading ? ' loading' : ''}`;
  if (c.disabled || c.loading) el.disabled = true;
  if (c.tooltip) el.title = c.tooltip;
  if (c.icon && (c.icon_position || 'leading') === 'leading') el.appendChild(renderIcon(c.icon));
  el.appendChild(document.createTextNode(' ' + (c.label || '')));
  if (c.icon && c.icon_position === 'trailing') el.appendChild(renderIcon(c.icon));
  if (c.on_click) {
    el.addEventListener('click', () => dispatchAction(ctx, null, c.on_click, c.params || {}));
  }
  return el;
}

function renderIconButton(c, ctx) {
  const el = document.createElement('button');
  el.type = 'button';
  el.className = `sdk-icon-button variant-${c.variant || 'secondary'} ${c.size || 'md'}`;
  if (c.disabled || c.loading) el.disabled = true;
  if (c.tooltip) el.title = c.tooltip;
  if (c.aria_label) el.setAttribute('aria-label', c.aria_label);
  el.appendChild(renderIcon(c.icon));
  if (c.on_click) el.addEventListener('click', () => dispatchAction(ctx, null, c.on_click, c.params || {}));
  return el;
}

function renderButtonGroup(c, ctx) {
  const el = document.createElement('div');
  el.className = `sdk-button-group${c.attached ? ' attached' : ''}`;
  for (const b of c.buttons || []) {
    const n = renderComponent(b, ctx);
    if (n) el.appendChild(n);
  }
  return el;
}

function renderLink(c, ctx) {
  const el = document.createElement(c.url ? 'a' : 'button');
  el.className = `sdk-link ${c.variant || 'default'}`;
  if (c.url) {
    el.href = c.url;
    if (/^https?:\/\//.test(c.url)) { el.target = '_blank'; el.rel = 'noopener'; }
  } else {
    el.type = 'button';
  }
  if (c.icon) el.appendChild(renderIcon(c.icon));
  el.appendChild(document.createTextNode(' ' + (c.label || '')));
  if (c.panel_id) el.addEventListener('click', (ev) => { ev.preventDefault(); navigateToPanel(ctx, c.panel_id); });
  else if (c.on_click) el.addEventListener('click', () => dispatchAction(ctx, null, c.on_click, {}));
  return el;
}

function renderMenu(c, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-menu-wrap';
  const trigger = renderComponent(c.trigger, ctx);
  if (trigger) {
    trigger.addEventListener('click', (ev) => { ev.stopPropagation(); toggleMenu(menu); });
    wrap.appendChild(trigger);
  }
  const menu = document.createElement('div');
  menu.className = 'sdk-menu';
  menu.hidden = true;
  for (const item of c.items || []) {
    if (item.divider_before) {
      const sep = document.createElement('div');
      sep.className = 'sdk-menu-divider';
      menu.appendChild(sep);
    }
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = `sdk-menu-item${item.destructive ? ' destructive' : ''}${item.disabled ? ' disabled' : ''}`;
    if (item.disabled) btn.disabled = true;
    if (item.icon) btn.appendChild(renderIcon(item.icon));
    btn.appendChild(document.createTextNode(' ' + (item.label || '')));
    if (item.shortcut) {
      const s = document.createElement('span');
      s.className = 'sdk-menu-shortcut';
      s.textContent = item.shortcut;
      btn.appendChild(s);
    }
    if (item.on_click) {
      btn.addEventListener('click', () => { menu.hidden = true; dispatchAction(ctx, null, item.on_click, { itemId: item.id }); });
    }
    menu.appendChild(btn);
  }
  wrap.appendChild(menu);
  document.addEventListener('click', () => { menu.hidden = true; });
  return wrap;
}

function toggleMenu(menuEl) { menuEl.hidden = !menuEl.hidden; }

function renderActionBar(c, ctx) {
  const el = document.createElement('div');
  el.className = `sdk-action-bar ${c.align || 'space-between'}`;
  const left = document.createElement('div');
  left.className = 'sdk-action-bar-group';
  for (const a of c.primary || []) { const n = renderComponent(a, ctx); if (n) left.appendChild(n); }
  el.appendChild(left);
  const right = document.createElement('div');
  right.className = 'sdk-action-bar-group';
  for (const a of c.secondary || []) { const n = renderComponent(a, ctx); if (n) right.appendChild(n); }
  el.appendChild(right);
  return el;
}

function renderFilterChips(c, ctx) {
  const el = document.createElement('div');
  el.className = 'sdk-filter-chips';
  for (const chip of c.chips || []) {
    const ch = document.createElement('span');
    ch.className = 'sdk-chip';
    if (chip.on_click) {
      ch.classList.add('clickable');
      ch.addEventListener('click', () => dispatchAction(ctx, null, chip.on_click, { chipId: chip.id }));
    }
    if (chip.icon) ch.appendChild(renderIcon(chip.icon));
    ch.appendChild(document.createTextNode(chip.label || ''));
    if (chip.removable && chip.on_remove) {
      const x = document.createElement('button');
      x.type = 'button';
      x.className = 'sdk-chip-dismiss';
      x.textContent = '×';
      x.addEventListener('click', (ev) => { ev.stopPropagation(); dispatchAction(ctx, null, chip.on_remove, { chipId: chip.id }); });
      ch.appendChild(x);
    }
    el.appendChild(ch);
  }
  if (c.on_clear_all) {
    const clr = document.createElement('button');
    clr.type = 'button';
    clr.className = 'sdk-filter-clear-all';
    clr.textContent = 'Wyczyść';
    clr.addEventListener('click', () => dispatchAction(ctx, null, c.on_clear_all, {}));
    el.appendChild(clr);
  }
  return el;
}

function renderWizardFooter(c, ctx) {
  const el = document.createElement('footer');
  el.className = 'sdk-wizard-footer';
  const left = document.createElement('div');
  if (c.on_cancel) {
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'sdk-button variant-ghost';
    cancel.textContent = c.cancel_label || 'Anuluj';
    cancel.addEventListener('click', () => dispatchAction(ctx, null, c.on_cancel, {}));
    left.appendChild(cancel);
  }
  el.appendChild(left);
  const mid = document.createElement('div');
  if (c.step_label) {
    const s = document.createElement('span');
    s.className = 'sdk-wizard-step-label';
    s.textContent = c.step_label;
    mid.appendChild(s);
  }
  el.appendChild(mid);
  const right = document.createElement('div');
  right.style.display = 'flex';
  right.style.gap = 'var(--sdk-spacing-sm)';
  if (c.on_back) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'sdk-button variant-secondary';
    b.textContent = c.back_label || 'Wstecz';
    b.addEventListener('click', () => dispatchAction(ctx, null, c.on_back, {}));
    right.appendChild(b);
  }
  if (c.on_next) {
    const n = document.createElement('button');
    n.type = 'button';
    n.className = 'sdk-button';
    n.textContent = c.next_label || 'Dalej';
    if (c.next_disabled) n.disabled = true;
    n.addEventListener('click', () => dispatchAction(ctx, null, c.on_next, {}));
    right.appendChild(n);
  }
  el.appendChild(right);
  return el;
}

// =============================================================================
// Specialized
// =============================================================================

function pxFromSize(size, fallback) {
  if (!size || typeof size !== 'object') return fallback;
  if (size.kind === 'fixed' && size.unit && size.unit.kind === 'px') return Number(size.unit.value) || fallback;
  return fallback;
}

function renderCanvasSpecialized(c, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-canvas-wrap';
  const cv = document.createElement('tf-canvas');
  cv.width = pxFromSize(c.width, 600);
  cv.height = pxFromSize(c.height, 300);
  cv.cursor = c.cursor || 'default';
  if (c.background) cv.background = c.background;
  if (c.on_pointer_throttle_ms) cv.pointerThrottleMs = c.on_pointer_throttle_ms;
  cv.commands = Array.isArray(c.commands) ? c.commands : [];
  if (c.on_pointer) {
    cv.onPointer = (evt) => dispatchAction(ctx, null, c.on_pointer, evt);
  }
  wrap.appendChild(cv);
  return wrap;
}

function renderSparkline(c) {
  const el = document.createElement('tf-sparkline');
  el.points = c.points || [];
  if (c.color) el.color = c.color;
  if (c.height) el.height = c.height;
  el.fill = Boolean(c.fill);
  el.showDots = Boolean(c.show_dots);
  return el;
}

function renderStackedBar(c) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-stacked-bar';
  const colors = (c.colors && c.colors.length) ? c.colors : ['primary', 'accent', 'success', 'warning', 'danger', 'info'];
  for (const row of c.data || []) {
    const r = document.createElement('div');
    r.className = 'sdk-stacked-bar-row';
    const lbl = document.createElement('span');
    lbl.className = 'label';
    lbl.textContent = row.label || '';
    r.appendChild(lbl);
    const track = document.createElement('div');
    track.className = 'sdk-stacked-bar-track';
    const total = row.total || (row.values || []).reduce((a, b) => a + Number(b), 0) || 1;
    (row.values || []).forEach((v, i) => {
      const seg = document.createElement('div');
      seg.className = 'sdk-stacked-bar-seg';
      seg.style.width = `${(Number(v) / total) * 100}%`;
      seg.style.background = `var(--sdk-color-${(colors[i % colors.length] || 'primary').replace(/_/g, '-')})`;
      if (c.show_values) seg.title = String(v);
      track.appendChild(seg);
    });
    r.appendChild(track);
    wrap.appendChild(r);
  }
  if (c.show_legend && (c.data || []).length) {
    const lg = document.createElement('div');
    lg.className = 'sdk-stacked-bar-legend';
    const seriesCount = ((c.data[0] && c.data[0].values) || []).length;
    for (let i = 0; i < seriesCount; i++) {
      const item = document.createElement('span');
      const dot = document.createElement('span');
      dot.className = 'sdk-stacked-bar-legend-dot';
      dot.style.background = `var(--sdk-color-${(colors[i % colors.length] || 'primary').replace(/_/g, '-')})`;
      item.appendChild(dot);
      item.appendChild(document.createTextNode(`series ${i + 1}`));
      lg.appendChild(item);
    }
    wrap.appendChild(lg);
  }
  return wrap;
}

function renderHeatmapSpecialized(c, ctx) {
  const el = document.createElement('tf-heatmap');
  el.values = c.values || [];
  if (c.row_labels) el.rowLabels = c.row_labels;
  if (c.col_labels) el.colLabels = c.col_labels;
  if (c.color_scale) el.colorScale = c.color_scale;
  if (c.on_cell_click) {
    el.onCellClick = (evt) => dispatchAction(ctx, null, c.on_cell_click, evt);
  }
  return el;
}

function renderAccessMatrix(c, ctx) {
  const table = document.createElement('table');
  table.className = 'sdk-access-matrix';
  const thead = document.createElement('thead');
  const trH = document.createElement('tr');
  trH.appendChild(document.createElement('th'));
  for (const role of c.roles || []) {
    const th = document.createElement('th');
    th.textContent = role;
    trH.appendChild(th);
  }
  thead.appendChild(trH);
  table.appendChild(thead);
  const tbody = document.createElement('tbody');
  for (const res of c.resources || []) {
    const tr = document.createElement('tr');
    const td = document.createElement('td');
    td.textContent = res.label || res.id;
    tr.appendChild(td);
    for (const role of c.roles || []) {
      const cell = document.createElement('td');
      const perm = (res.permissions || []).find((p) => p.role === role);
      const granted = Boolean(perm && perm.granted);
      cell.className = granted ? 'sdk-access-cell-on' : 'sdk-access-cell-off';
      cell.textContent = granted ? '✓' : '–';
      if (!c.readonly && c.on_toggle && !(perm && perm.disabled)) {
        cell.style.cursor = 'pointer';
        cell.addEventListener('click', () => dispatchAction(ctx, null, c.on_toggle, {
          resourceId: res.id, role, granted: !granted,
        }));
      }
      tr.appendChild(cell);
    }
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  return table;
}

function renderWeeklyScheduleGrid(c, ctx) {
  const el = document.createElement('tf-heatmap');
  el.values = c.values || [];
  el.rowLabels = ['Pn', 'Wt', 'Śr', 'Cz', 'Pt', 'Sb', 'Nd'];
  el.colLabels = Array.from({ length: 24 }, (_, i) => String(i));
  el.colorScale = c.mode === 'toggle' ? 'categorical' : 'sequential';
  if (!c.readonly && c.on_cell_click) {
    el.onCellClick = (evt) => dispatchAction(ctx, null, c.on_cell_click, evt);
  }
  return el;
}

function renderVideoTile(c, ctx) {
  const wrap = document.createElement('div');
  wrap.className = 'sdk-video-tile-wrap';
  const stream = document.createElement('tf-video-stream');
  if (c.stream_id) stream.setAttribute('stream-id', c.stream_id);
  if (c.label) stream.setAttribute('label', c.label);
  if (c.height_px) stream.setAttribute('height-px', String(c.height_px));
  wrap.appendChild(stream);
  if (Array.isArray(c.overlays) && c.overlays.length) {
    const layer = document.createElement('div');
    layer.className = 'sdk-video-overlay-layer';
    for (const ov of c.overlays) {
      const box = document.createElement('div');
      box.className = 'sdk-video-bbox';
      // Normalised 0..1 -> %, fallback do pixels
      const rect = ov.rect || {};
      const isNorm = rect.x <= 1 && rect.y <= 1 && rect.width <= 1 && rect.height <= 1;
      const u = (n) => isNorm ? `${n * 100}%` : `${n}px`;
      box.style.left = u(rect.x);
      box.style.top = u(rect.y);
      box.style.width = u(rect.width);
      box.style.height = u(rect.height);
      if (ov.color) box.style.borderColor = `var(--sdk-color-${ov.color.replace(/_/g, '-')})`;
      if (ov.label) {
        const lbl = document.createElement('span');
        lbl.className = 'sdk-video-bbox-label';
        lbl.textContent = `${ov.label}${ov.confidence != null ? ` ${(ov.confidence * 100).toFixed(0)}%` : ''}`;
        box.appendChild(lbl);
      }
      layer.appendChild(box);
    }
    wrap.appendChild(layer);
  }
  if (c.on_click) {
    wrap.style.cursor = 'pointer';
    wrap.addEventListener('click', () => dispatchAction(ctx, null, c.on_click, {}));
  }
  return wrap;
}

function renderWelcomeHero(c, ctx) {
  const el = document.createElement('section');
  el.className = 'sdk-welcome-hero';
  if (c.icon) el.appendChild(renderIcon(c.icon));
  const t = document.createElement('h1');
  t.className = 'sdk-welcome-hero-title';
  t.textContent = c.title || '';
  el.appendChild(t);
  if (c.subtitle) {
    const s = document.createElement('p');
    s.className = 'sdk-welcome-hero-subtitle';
    s.textContent = c.subtitle;
    el.appendChild(s);
  }
  if (Array.isArray(c.actions) && c.actions.length) {
    const a = document.createElement('div');
    a.className = 'sdk-welcome-hero-actions';
    for (const ac of c.actions) {
      const n = renderComponent(ac, ctx);
      if (n) a.appendChild(n);
    }
    el.appendChild(a);
  }
  return el;
}

function renderStepProgress(c) {
  const el = document.createElement('div');
  el.className = `sdk-step-progress${c.orientation === 'vertical' ? ' vertical' : ''}`;
  (c.steps || []).forEach((step, i) => {
    const s = document.createElement('div');
    s.className = `sdk-step ${step.status || 'pending'}${i === c.active_index ? ' active' : ''}`;
    const dot = document.createElement('span');
    dot.className = 'sdk-step-dot';
    if (step.status === 'done') dot.textContent = '✓';
    else if (step.status === 'error') dot.textContent = '!';
    else dot.textContent = String(i + 1);
    s.appendChild(dot);
    s.appendChild(document.createTextNode(' ' + (step.label || '')));
    el.appendChild(s);
    if (i < (c.steps || []).length - 1) {
      const sep = document.createElement('span');
      sep.className = 'sdk-step-sep';
      el.appendChild(sep);
    }
  });
  return el;
}

function renderReqCard(c, ctx) {
  const el = document.createElement('article');
  el.className = `sdk-req-card${c.decision ? ' ' + c.decision : ''}`;
  const head = document.createElement('header');
  head.className = 'sdk-req-card-head';
  if (c.addon_icon) head.appendChild(renderIcon(c.addon_icon));
  const lbl = document.createElement('strong');
  lbl.textContent = c.addon_label;
  head.appendChild(lbl);
  if (c.required) {
    const r = document.createElement('span');
    r.className = 'sdk-badge tone-warning sm';
    r.textContent = 'Required';
    head.appendChild(r);
  }
  if (c.public) {
    const p = document.createElement('span');
    p.className = 'sdk-badge tone-info sm';
    p.textContent = 'Public';
    head.appendChild(p);
  }
  el.appendChild(head);
  const perm = document.createElement('div');
  perm.className = 'sdk-text-body-strong';
  perm.textContent = c.permission || '';
  el.appendChild(perm);
  if (c.description) {
    const d = document.createElement('p');
    d.className = 'sdk-text-caption';
    d.style.color = 'var(--sdk-color-text-muted)';
    d.textContent = c.description;
    el.appendChild(d);
  }
  if (!c.decision || c.decision === 'pending') {
    const actions = document.createElement('div');
    actions.className = 'sdk-req-card-actions';
    if (c.on_reject) {
      const rj = document.createElement('button');
      rj.type = 'button';
      rj.className = 'sdk-button variant-ghost sm';
      rj.textContent = 'Odmów';
      rj.addEventListener('click', () => dispatchAction(ctx, null, c.on_reject, {}));
      actions.appendChild(rj);
    }
    if (c.on_accept) {
      const ac = document.createElement('button');
      ac.type = 'button';
      ac.className = 'sdk-button sm';
      ac.textContent = 'Zezwól';
      ac.addEventListener('click', () => dispatchAction(ctx, null, c.on_accept, {}));
      actions.appendChild(ac);
    }
    el.appendChild(actions);
  }
  return el;
}

function renderDecisionRow(c, ctx) {
  const el = document.createElement('div');
  el.className = 'sdk-decision-row';
  if (c.accent) el.style.borderLeft = `3px solid var(--sdk-color-${c.accent.replace(/_/g, '-')})`;
  const body = document.createElement('div');
  const t = document.createElement('div');
  t.className = 'sdk-text-body-strong';
  t.textContent = c.label;
  body.appendChild(t);
  if (c.description) {
    const d = document.createElement('div');
    d.className = 'sdk-text-caption';
    d.style.color = 'var(--sdk-color-text-muted)';
    d.textContent = c.description;
    body.appendChild(d);
  }
  el.appendChild(body);
  const acts = document.createElement('div');
  acts.className = 'sdk-decision-row-actions';
  if (c.on_reject) {
    const rj = document.createElement('button');
    rj.type = 'button';
    rj.className = `sdk-button variant-ghost sm${c.decision === 'rejected' ? ' active' : ''}`;
    rj.textContent = '✗';
    rj.addEventListener('click', () => dispatchAction(ctx, null, c.on_reject, { id: c.id }));
    acts.appendChild(rj);
  }
  if (c.on_accept) {
    const ac = document.createElement('button');
    ac.type = 'button';
    ac.className = `sdk-button sm${c.decision === 'accepted' ? ' variant-success' : ' variant-ghost'}`;
    ac.textContent = '✓';
    ac.addEventListener('click', () => dispatchAction(ctx, null, c.on_accept, { id: c.id }));
    acts.appendChild(ac);
  }
  el.appendChild(acts);
  return el;
}

function renderAlarmFeed(c, ctx) {
  const el = document.createElement('tf-alarm-feed');
  if (c.stream_id) el.setAttribute('stream-id', c.stream_id);
  if (c.max_items) el.setAttribute('max-items', String(c.max_items));
  if (c.height_px) el.setAttribute('height-px', String(c.height_px));
  if (c.on_item_click) {
    el.onItemClick = (raw) => dispatchAction(ctx, null, c.on_item_click, raw || {});
  }
  return el;
}

function renderFpsCounter(c) {
  const el = document.createElement('tf-fps-counter');
  if (c.stream_id) el.setAttribute('stream-id', c.stream_id);
  if (c.label) el.setAttribute('label', c.label);
  if (c.format) el.setAttribute('format', c.format);
  if (c.show_sparkline) el.setAttribute('show-sparkline', '');
  return el;
}

// =============================================================================
// Legacy renderers (zachowane bez zmian funkcjonalnych — uzywane przez Legacy::*
// payloady sprzed Chunka 2.1)
// =============================================================================

function renderLegacyText(c) {
  const el = document.createElement('div');
  el.className = 'addon-text';
  const styleKey = String(c.style ?? '').trim().toLowerCase();
  const TEXT_STYLE_CLASSES = {
    muted: 'addon-text-muted', bold: 'addon-text-bold', italic: 'addon-text-italic',
    error: 'addon-text-error', success: 'addon-text-success', warning: 'addon-text-warning',
  };
  if (TEXT_STYLE_CLASSES[styleKey]) el.classList.add(TEXT_STYLE_CLASSES[styleKey]);
  el.textContent = c.content ?? '';
  return el;
}

function renderLegacyInput(c, ctx) {
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

function renderLegacyButton(c, ctx) {
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

function renderLegacySelect(c, ctx) {
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

function renderLegacyTable(c) {
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
    for (let i = 0; i < keys.length; i++) obj[keys[i]] = row[i] ?? '';
    return obj;
  });
  queueMicrotask(() => { el.rows = rowsData; });
  return el;
}

function renderLegacyTabs(c, ctx) {
  // Stary tabs handler — uzywany przez Legacy::Tabs (a nie Container::Tabs).
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
  tabsNav.addEventListener('tf-tab-click', (ev) => {
    const activeId = ev.detail?.id;
    if (!activeId) return;
    panes.querySelectorAll('[data-tab-pane]').forEach((p) => { p.hidden = p.dataset.tabPane !== activeId; });
  });
  wrap.appendChild(tabsNav);
  wrap.appendChild(panes);
  return wrap;
}

function renderLegacyImage(c) {
  const el = document.createElement('img');
  el.className = 'addon-image';
  const rawSrc = String(c.src ?? '');
  if (isSafeImageSrc(rawSrc)) el.setAttribute('src', rawSrc);
  else if (rawSrc) console.warn('[addon-app] dropped unsafe src:', rawSrc);
  el.setAttribute('alt', String(c.alt ?? ''));
  if (c.width) el.setAttribute('width', String(c.width));
  if (c.height) el.setAttribute('height', String(c.height));
  return el;
}

function renderLegacyList(c, ctx) {
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

function renderLegacyForm(c, ctx) {
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
  submit.addEventListener('click', (ev) => {
    ev.preventDefault();
    form.dispatchEvent(new Event('submit', { cancelable: true }));
  });
  return form;
}

function renderLegacyProgress(c) {
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

function renderLegacyCode(c) {
  const pre = document.createElement('pre');
  pre.className = 'addon-code';
  const code = document.createElement('code');
  code.className = `language-${(c.language ?? 'plain').replace(/[^a-z0-9_-]/gi, '')}`;
  code.textContent = c.content ?? '';
  pre.appendChild(code);
  return pre;
}

function renderLegacyBadge(c) {
  const el = document.createElement('tf-chip');
  el.textContent = c.text ?? '';
  const color = (c.color ?? '').toLowerCase();
  const status = ({
    green: 'success', success: 'success', red: 'danger', danger: 'danger',
    error: 'danger', yellow: 'warning', orange: 'warning', warning: 'warning',
    blue: 'info', info: 'info',
  })[color] || 'info';
  el.setAttribute('status', status);
  return el;
}

function renderLiveCameraTile(c, ctx) {
  const el = document.createElement('tf-live-camera-tile');
  const cameraId = String(c.camera_id ?? c.cameraId ?? '');
  if (cameraId) el.setAttribute('camera-id', cameraId);
  const ttl = Number(c.ttl_secs ?? c.ttlSecs);
  if (Number.isFinite(ttl) && ttl > 0) el.setAttribute('ttl-secs', String(ttl));
  const label = c.label;
  if (typeof label === 'string' && label.length > 0) el.setAttribute('label', label);
  const heightPx = Number(c.height_px ?? c.heightPx);
  if (Number.isFinite(heightPx) && heightPx > 0) el.setAttribute('height-px', String(heightPx));
  if (ctx?.addonId) el.setAttribute('addon-id', ctx.addonId);
  if (ctx?.panelId) el.setAttribute('panel-id', ctx.panelId);
  return el;
}

function renderVideoStream(c) {
  const el = document.createElement('tf-video-stream');
  const streamId = String(c.stream_id ?? c.streamId ?? '');
  if (streamId) el.setAttribute('stream-id', streamId);
  const label = c.label;
  if (typeof label === 'string' && label.length > 0) el.setAttribute('label', label);
  const heightPx = Number(c.height_px ?? c.heightPx);
  if (Number.isFinite(heightPx) && heightPx > 0) el.setAttribute('height-px', String(heightPx));
  return el;
}

function renderUnknown(typeName) {
  const el = document.createElement('div');
  el.className = 'sdk-unknown';
  el.textContent = `[unknown component: ${typeName}]`;
  return el;
}

// =============================================================================
// Action dispatch / navigation
// =============================================================================

async function navigateToPanel(ctx, panelId) {
  if (!panelId || !ctx?.addonId) return;
  // Wymieniamy panel w miejscu — preferowane przy zmianie zakladki w obrebie
  // tego samego addona. Router obsluguje cross-addon nav jesli kiedys bedzie
  // potrzebne.
  try {
    const Router = (await import('/js/router.js')).default;
    Router.navigate('addon-app', { addonId: ctx.addonId, panelId });
  } catch {
    // Fallback — bezposrednie odswiezenie panelu
    await refreshPanel(ctx.addonId, panelId);
  }
}

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
    const shell = document.querySelector('.addon-app-shell');
    if (shell) {
      const banner = document.createElement('div');
      banner.className = 'addon-action-error';
      banner.textContent = `Akcja "${actionId}" nie powiodla sie: ${e.message}`;
      shell.insertBefore(banner, shell.firstChild);
    }
    return;
  }
  await refreshPanel(ctx.addonId, ctx.panelId);
}

function collectFormValues(formEl) {
  const out = {};
  if (!formEl) return out;
  formEl.querySelectorAll('[data-field-id]').forEach((el) => {
    const id = el.dataset.fieldId;
    if (!id) return;
    let v;
    if (el.type === 'checkbox' || el.type === 'radio') v = el.checked;
    else if ('value' in el) v = el.value;
    else v = el.getAttribute('value') ?? '';
    out[id] = v;
  });
  return out;
}

// =============================================================================
// Helpers
// =============================================================================

function isSafeImageSrc(src) {
  if (!src) return false;
  if (src.startsWith('/') && !src.startsWith('//')) return true;
  if (src.startsWith('data:image/')) return true;
  try {
    const url = new URL(src, window.location.href);
    if (url.protocol === 'https:' && url.origin === window.location.origin) return true;
  } catch {
    return false;
  }
  return false;
}

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
