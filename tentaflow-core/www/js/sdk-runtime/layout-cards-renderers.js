// =============================================================================
// Plik: sdk-runtime/layout-cards-renderers.js
// Opis: Rendererzy 4 cards Layout (Faza 6 Krok 3.3a-3):
//   - Card        (tag 0x0106) — generic container z padding/radius/shadow
//   - SectionCard (tag 0x0107) — Card z built-in title/subtitle/header_actions
//   - Collapsible (tag 0x010D) — expandable section, handler "open"/"close"
//   - Accordion   (tag 0x010E) — multi-Collapsible z mutex mode
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/layout/cards.rs`.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import {
  resolveBindRef,
  subscribeBindRef,
} from './bind-resolver.js';

// =============================================================================
// Token whitelisty (spec §1.5)
// =============================================================================

const SPACINGS = new Set([
  'zero', 'xxs', 'xs', 'sm', 'md', 'lg', 'xl', 'xxl',
]);
const CARD_VARIANTS = new Set(['filled', 'outlined', 'elevated', 'ghost']);
const RADIUS_TOKENS = new Set([
  'none', 'xs', 'sm', 'md', 'lg', 'xl', 'pill', 'circle',
]);
const SHADOW_TOKENS = new Set([
  'none', 'subtle', 'medium', 'elevated', 'floating',
]);
const BACKGROUND_TOKENS = new Set([
  'none', 'subtle', 'muted', 'accent', 'inverse',
]);
const TONES = new Set([
  'neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted',
]);
const ACCORDION_MODES = new Set(['single', 'multiple']);

function requireEnum(value, set, ctx) {
  if (typeof value !== 'string' || !set.has(value)) {
    throw new TypeError(
      `${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(value)}`
    );
  }
  return value;
}

function optionalEnum(value, set, ctx) {
  if (value === undefined || value === null) return undefined;
  return requireEnum(value, set, ctx);
}

function requireBool(value, ctx) {
  if (typeof value !== 'boolean') {
    throw new TypeError(`${ctx}: expected boolean, got ${typeof value}`);
  }
  return value;
}

function requireArray(value, ctx) {
  if (!Array.isArray(value)) {
    throw new TypeError(`${ctx}: expected Array, got ${typeof value}`);
  }
  return value;
}

function requireString(value, ctx) {
  if (typeof value !== 'string') {
    throw new TypeError(`${ctx}: expected string, got ${typeof value}`);
  }
  return value;
}

function assertOnlyKnownFields(fields, allowedKeys, componentName) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(
        `${componentName}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`
      );
    }
  }
}

function assertOnlyKnownObjectKeys(obj, allowedKeys, ctx) {
  for (const k of Object.keys(obj)) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${ctx}: unexpected key '${k}'`);
    }
  }
}

/// BorderToken z `inline.rs` — tagged union `none/hairline/thin/strong/accent{tone}`.
/// Walidacja per-variant + zwrócenie CSS class fragment.
function borderTokenToClass(border, ctx) {
  if (!border || typeof border !== 'object') {
    throw new TypeError(`${ctx}: BorderToken must be object`);
  }
  switch (border.kind) {
    case 'none':
      assertOnlyKnownObjectKeys(border, new Set(['kind']), `${ctx}.none`);
      return 'tf-card--border-none';
    case 'hairline':
      assertOnlyKnownObjectKeys(border, new Set(['kind']), `${ctx}.hairline`);
      return 'tf-card--border-hairline';
    case 'thin':
      assertOnlyKnownObjectKeys(border, new Set(['kind']), `${ctx}.thin`);
      return 'tf-card--border-thin';
    case 'strong':
      assertOnlyKnownObjectKeys(border, new Set(['kind']), `${ctx}.strong`);
      return 'tf-card--border-strong';
    case 'accent': {
      assertOnlyKnownObjectKeys(border, new Set(['kind', 'tone']), `${ctx}.accent`);
      const tone = requireEnum(border.tone, TONES, `${ctx}.accent.tone`);
      return `tf-card--border-accent tf-card--border-tone-${tone}`;
    }
    default:
      throw new TypeError(`${ctx}.kind unsupported: ${border.kind}`);
  }
}

/// Zastosuj wszystkie wspólne klasy Card-style do `el`. Używane przez
/// Card i SectionCard (oba mają ten sam set tokenów).
function applyCardClasses(el, opts, ctxLabel) {
  el.classList.add(`tf-card--variant-${opts.variant}`);
  el.classList.add(`tf-card--padding-${opts.padding}`);
  el.classList.add(`tf-card--gap-${opts.gap}`);
  el.classList.add(`tf-card--radius-${opts.radius}`);
  el.classList.add(`tf-card--shadow-${opts.shadow}`);
  el.classList.add(`tf-card--bg-${opts.background}`);
  if (opts.accent) el.classList.add(`tf-card--accent-${opts.accent}`);
  for (const cls of opts.borderClasses.split(' ')) {
    if (cls) el.classList.add(cls);
  }
}

// =============================================================================
// Card (0x0106)
// =============================================================================

export const CARD_TAG = 0x0106;
const CARD_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);

function renderCard(component, ctx) {
  assertOnlyKnownFields(component.fields, CARD_FIELD_KEYS, 'Card');
  const variant = requireEnum(
    ctx.readField(component.fields, 0),
    CARD_VARIANTS,
    'Card.variant'
  );
  // §3 0x0106 defaults: padding="lg", gap="md", radius="lg",
  // shadow=Elevated→subtle/inne→none.
  const padding = optionalEnum(
    ctx.readField(component.fields, 1),
    SPACINGS,
    'Card.padding'
  ) ?? 'lg';
  const gap = optionalEnum(
    ctx.readField(component.fields, 2),
    SPACINGS,
    'Card.gap'
  ) ?? 'md';
  const radius = optionalEnum(
    ctx.readField(component.fields, 3),
    RADIUS_TOKENS,
    'Card.radius'
  ) ?? 'lg';
  const shadowRaw = ctx.readField(component.fields, 4);
  const shadow =
    shadowRaw === undefined
      ? (variant === 'elevated' ? 'subtle' : 'none')
      : requireEnum(shadowRaw, SHADOW_TOKENS, 'Card.shadow');
  const borderRaw = ctx.readField(component.fields, 5);
  if (borderRaw === undefined) {
    throw new TypeError('Card.border is required');
  }
  const borderClasses = borderTokenToClass(borderRaw, 'Card.border');
  const background = requireEnum(
    ctx.readField(component.fields, 6),
    BACKGROUND_TOKENS,
    'Card.background'
  );
  const accent = optionalEnum(
    ctx.readField(component.fields, 7),
    TONES,
    'Card.accent'
  );
  const childrenRaw = ctx.readField(component.fields, 8);
  const children = childrenRaw === undefined ? [] : requireArray(childrenRaw, 'Card.children');
  const interactiveRaw = ctx.readField(component.fields, 9);
  if (interactiveRaw === undefined) {
    throw new TypeError('Card.interactive is required');
  }
  const interactive = requireBool(interactiveRaw, 'Card.interactive');
  const clickableRaw = ctx.readField(component.fields, 10);
  if (clickableRaw === undefined) {
    throw new TypeError('Card.clickable is required');
  }
  const clickable = requireBool(clickableRaw, 'Card.clickable');

  const el = document.createElement('div');
  el.classList.add('tf-card');
  applyCardClasses(el, {
    variant, padding, gap, radius, shadow, background, accent, borderClasses,
  });
  if (interactive) el.classList.add('tf-card--interactive');
  if (clickable) {
    el.classList.add('tf-card--clickable');
    el.setAttribute('role', 'button');
    el.setAttribute('tabindex', '0');
    // A11y: na div'ie z role=button klawiatura NIE generuje natywnego
    // click'a — Enter/Space muszą explicite go syntetyzować, inaczej
    // user klawiaturowy widzi focusable element bez aktywacji.
    const onKey = (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        el.click();
      }
    };
    el.addEventListener('keydown', onKey);
    ctx.registerCleanup(() => el.removeEventListener('keydown', onKey));
  }
  for (const childComponent of children) {
    el.appendChild(ctx.renderChild(childComponent));
  }
  return el;
}

// =============================================================================
// SectionCard (0x0107)
// =============================================================================

export const SECTION_CARD_TAG = 0x0107;
/// Spec §4 0x0200 Button. Walidujemy tag w SectionCard.header_actions bez
/// importu renderer'a Button (chunk 3.3b doda jego implementację) —
/// constraint jest egzekwowany na poziomie tag-u, niezależnie od tego,
/// czy renderer Button-a jest jeszcze zarejestrowany.
const BUTTON_TAG = 0x0200;
const SECTION_CARD_FIELD_KEYS = new Set([
  0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13,
]);

function renderSectionCard(component, ctx) {
  assertOnlyKnownFields(component.fields, SECTION_CARD_FIELD_KEYS, 'SectionCard');
  const titleBind = ctx.readField(component.fields, 0);
  if (titleBind == null) {
    throw new TypeError('SectionCard.title is required (BindRef)');
  }
  const subtitleBind = ctx.readField(component.fields, 1); // Option<BindRef>
  const headerActionsRaw = ctx.readField(component.fields, 2);
  const headerActions =
    headerActionsRaw === undefined
      ? []
      : requireArray(headerActionsRaw, 'SectionCard.header_actions');
  const headerDividerRaw = ctx.readField(component.fields, 3);
  if (headerDividerRaw === undefined) {
    throw new TypeError('SectionCard.header_divider is required');
  }
  const headerDivider = requireBool(
    headerDividerRaw,
    'SectionCard.header_divider'
  );
  const bodyRaw = ctx.readField(component.fields, 4);
  const body =
    bodyRaw === undefined ? [] : requireArray(bodyRaw, 'SectionCard.body');
  const footerRaw = ctx.readField(component.fields, 5);
  const footer =
    footerRaw == null ? null : requireArray(footerRaw, 'SectionCard.footer');
  const padding =
    optionalEnum(
      ctx.readField(component.fields, 6),
      SPACINGS,
      'SectionCard.padding'
    ) ?? 'lg';
  const gap =
    optionalEnum(
      ctx.readField(component.fields, 7),
      SPACINGS,
      'SectionCard.gap'
    ) ?? 'md';
  const variant = requireEnum(
    ctx.readField(component.fields, 8),
    CARD_VARIANTS,
    'SectionCard.variant'
  );
  const radius =
    optionalEnum(
      ctx.readField(component.fields, 9),
      RADIUS_TOKENS,
      'SectionCard.radius'
    ) ?? 'lg';
  const shadow =
    optionalEnum(
      ctx.readField(component.fields, 10),
      SHADOW_TOKENS,
      'SectionCard.shadow'
    ) ?? 'subtle';
  const borderRaw = ctx.readField(component.fields, 11);
  if (borderRaw === undefined) {
    throw new TypeError('SectionCard.border is required');
  }
  const borderClasses = borderTokenToClass(borderRaw, 'SectionCard.border');
  const background = requireEnum(
    ctx.readField(component.fields, 12),
    BACKGROUND_TOKENS,
    'SectionCard.background'
  );
  const accent = optionalEnum(
    ctx.readField(component.fields, 13),
    TONES,
    'SectionCard.accent'
  );

  const el = document.createElement('div');
  el.classList.add('tf-card');
  el.classList.add('tf-section-card');
  applyCardClasses(el, {
    variant, padding, gap, radius, shadow, background, accent, borderClasses,
  });

  // Header (title + subtitle + actions + optional divider).
  const header = document.createElement('div');
  header.classList.add('tf-section-card__header');
  const titles = document.createElement('div');
  titles.classList.add('tf-section-card__titles');
  const titleEl = document.createElement('h3');
  titleEl.classList.add('tf-section-card__title');
  titles.appendChild(titleEl);
  bindTextContent(titleEl, titleBind, ctx);
  if (subtitleBind != null) {
    const subEl = document.createElement('div');
    subEl.classList.add('tf-section-card__subtitle');
    titles.appendChild(subEl);
    bindTextContent(subEl, subtitleBind, ctx);
  }
  header.appendChild(titles);

  if (headerActions.length > 0) {
    const actions = document.createElement('div');
    actions.classList.add('tf-section-card__actions');
    for (const action of headerActions) {
      // Spec §3 0x0107: header_actions to `Vec<ComponentRef<Button>>` —
      // tylko komponenty z tag-em 0x0200 (Button) są dozwolone. Mirror
      // Rust `ensure_ref_tag_encode/decode(Button::TAG)`.
      if (!action || typeof action !== 'object' || action.tag !== BUTTON_TAG) {
        throw new TypeError(
          `SectionCard.header_actions entry must be Button (tag 0x0200), got 0x${
            (action && action.tag != null ? action.tag : 0).toString(16).padStart(4, '0')
          }`
        );
      }
      actions.appendChild(ctx.renderChild(action));
    }
    header.appendChild(actions);
  }
  el.appendChild(header);
  if (headerDivider) {
    const divider = document.createElement('div');
    divider.classList.add('tf-section-card__header-divider');
    el.appendChild(divider);
  }

  // Body.
  const bodyEl = document.createElement('div');
  bodyEl.classList.add('tf-section-card__body');
  for (const childComponent of body) {
    bodyEl.appendChild(ctx.renderChild(childComponent));
  }
  el.appendChild(bodyEl);

  // Footer (opcjonalne).
  if (footer && footer.length > 0) {
    const footerEl = document.createElement('div');
    footerEl.classList.add('tf-section-card__footer');
    for (const childComponent of footer) {
      footerEl.appendChild(ctx.renderChild(childComponent));
    }
    el.appendChild(footerEl);
  }
  return el;
}

// =============================================================================
// Collapsible (0x010D)
// =============================================================================

export const COLLAPSIBLE_TAG = 0x010D;
const COLLAPSIBLE_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderCollapsible(component, ctx) {
  assertOnlyKnownFields(component.fields, COLLAPSIBLE_FIELD_KEYS, 'Collapsible');
  const headerComp = ctx.readField(component.fields, 0);
  if (headerComp == null || typeof headerComp !== 'object') {
    throw new TypeError('Collapsible.header must be Component');
  }
  const bodyRaw = ctx.readField(component.fields, 1);
  const body =
    bodyRaw === undefined ? [] : requireArray(bodyRaw, 'Collapsible.body');
  const expandedBind = ctx.readField(component.fields, 2);
  if (expandedBind == null) {
    throw new TypeError('Collapsible.expanded must be BindRef');
  }
  const animatedRaw = ctx.readField(component.fields, 3);
  if (animatedRaw === undefined) {
    throw new TypeError('Collapsible.animated is required');
  }
  const animated = requireBool(animatedRaw, 'Collapsible.animated');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-collapsible');
  if (animated) wrapper.classList.add('tf-collapsible--animated');

  const headerEl = document.createElement('div');
  headerEl.classList.add('tf-collapsible__header');
  headerEl.setAttribute('role', 'button');
  headerEl.setAttribute('tabindex', '0');
  headerEl.appendChild(ctx.renderChild(headerComp));
  wrapper.appendChild(headerEl);

  const bodyEl = document.createElement('div');
  bodyEl.classList.add('tf-collapsible__body');
  bodyEl.setAttribute('role', 'region');
  for (const childComponent of body) {
    bodyEl.appendChild(ctx.renderChild(childComponent));
  }
  wrapper.appendChild(bodyEl);

  const applyExpanded = () => {
    const v = resolveBindRef(expandedBind, ctx.store);
    const expanded = !!v;
    headerEl.setAttribute('aria-expanded', expanded ? 'true' : 'false');
    if (expanded) {
      bodyEl.removeAttribute('hidden');
      wrapper.classList.add('tf-collapsible--expanded');
    } else {
      bodyEl.setAttribute('hidden', '');
      wrapper.classList.remove('tf-collapsible--expanded');
    }
  };
  applyExpanded();
  const offExpanded = subscribeBindRef(expandedBind, ctx.store, applyExpanded);
  ctx.registerCleanup(offExpanded);

  // Toggle przez klik / Enter / Space na headerze emituje custom event
  // `open` lub `close` zgodnie z aktualnym stanem expanded. Addon
  // (przez component.handlers) decyduje czy zaktualizować store.
  // Engine attachuje listenery dla EventKind 'open'/'close' do wrapper'a;
  // my emitujemy CustomEvent na wrapper.
  const toggle = () => {
    const currentlyExpanded = resolveBindRef(expandedBind, ctx.store) === true;
    const eventName = currentlyExpanded ? 'close' : 'open';
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)(eventName, { bubbles: false })
    );
  };
  const onKey = (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      toggle();
    }
  };
  headerEl.addEventListener('click', toggle);
  headerEl.addEventListener('keydown', onKey);
  ctx.registerCleanup(() => {
    headerEl.removeEventListener('click', toggle);
    headerEl.removeEventListener('keydown', onKey);
  });
  return wrapper;
}

// =============================================================================
// Accordion (0x010E)
// =============================================================================

export const ACCORDION_TAG = 0x010E;
const ACCORDION_FIELD_KEYS = new Set([0, 1, 2]);
const ACCORDION_ITEM_KEYS = new Set(['id', 'header', 'body', 'default_expanded']);

function renderAccordion(component, ctx) {
  assertOnlyKnownFields(component.fields, ACCORDION_FIELD_KEYS, 'Accordion');
  const itemsRaw = ctx.readField(component.fields, 0);
  const items =
    itemsRaw === undefined ? [] : requireArray(itemsRaw, 'Accordion.items');
  const mode = requireEnum(
    ctx.readField(component.fields, 1),
    ACCORDION_MODES,
    'Accordion.mode'
  );
  const expandedIdsBind = ctx.readField(component.fields, 2);
  if (expandedIdsBind == null) {
    throw new TypeError('Accordion.expanded_ids must be BindRef');
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-accordion');
  wrapper.classList.add(`tf-accordion--mode-${mode}`);
  wrapper.setAttribute('role', 'region');

  // Renderujemy każdy item jako sekcję, sub-elementy zachowują nazwy
  // tf-accordion__item / __header / __body.
  const itemEls = [];
  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    assertOnlyKnownObjectKeys(item, ACCORDION_ITEM_KEYS, `Accordion.items[${i}]`);
    const itemId = requireString(item.id, `Accordion.items[${i}].id`);
    if (itemId.length === 0) {
      throw new TypeError(`Accordion.items[${i}].id must be non-empty`);
    }
    if (item.header == null || typeof item.header !== 'object') {
      throw new TypeError(`Accordion.items[${i}].header must be Component`);
    }
    const itemBody = requireArray(item.body, `Accordion.items[${i}].body`);
    requireBool(item.default_expanded, `Accordion.items[${i}].default_expanded`);

    const itemEl = document.createElement('div');
    itemEl.classList.add('tf-accordion__item');
    itemEl.setAttribute('data-accordion-id', itemId);

    const headerEl = document.createElement('div');
    headerEl.classList.add('tf-accordion__header');
    headerEl.setAttribute('role', 'button');
    headerEl.setAttribute('tabindex', '0');
    headerEl.appendChild(ctx.renderChild(item.header));
    itemEl.appendChild(headerEl);

    const bodyEl = document.createElement('div');
    bodyEl.classList.add('tf-accordion__body');
    bodyEl.setAttribute('role', 'region');
    for (const childComp of itemBody) {
      bodyEl.appendChild(ctx.renderChild(childComp));
    }
    itemEl.appendChild(bodyEl);

    const toggleItem = () => {
      const eventName = headerEl.getAttribute('aria-expanded') === 'true'
        ? 'close'
        : 'open';
      itemEl.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)(eventName, {
          bubbles: true,
          detail: { item_id: itemId },
        })
      );
    };
    const onKey = (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        toggleItem();
      }
    };
    headerEl.addEventListener('click', toggleItem);
    headerEl.addEventListener('keydown', onKey);
    ctx.registerCleanup(() => {
      headerEl.removeEventListener('click', toggleItem);
      headerEl.removeEventListener('keydown', onKey);
    });

    wrapper.appendChild(itemEl);
    itemEls.push({
      itemId, itemEl, headerEl, bodyEl,
      defaultExpanded: item.default_expanded === true,
    });
  }

  // Wstępny zbiór expanded ID-ów dla initial render, zanim BindRef
  // dostarczy realny stan: spec §3 0x010E pozwala każdemu `AccordionItem`
  // mieć `default_expanded=true`. Jeśli expanded_ids resolve daje
  // wartość, używamy jej; jeśli daje null/undefined/non-array, fallback
  // do per-item `default_expanded` flag'ów.
  const defaultExpandedIds = itemEls
    .filter((info) => info.defaultExpanded)
    .map((info) => info.itemId);

  const applyExpanded = () => {
    const raw = resolveBindRef(expandedIdsBind, ctx.store);
    let ids;
    if (Array.isArray(raw)) {
      ids = raw.filter((s) => typeof s === 'string');
    } else {
      // Brak/wadliwy stan w store → użyj default_expanded.
      ids = defaultExpandedIds.slice();
    }
    // Spec §3 0x010E mode=single: max 1 expanded. Renderer przycina do
    // pierwszego ID — zapobiega nieprawidłowemu state'owi dopóki addon
    // nie zaktualizuje store'a.
    if (mode === 'single' && ids.length > 1) {
      // eslint-disable-next-line no-console
      console.warn(
        `[accordion] mode='single' but expanded_ids has ${ids.length} entries — using first`
      );
      ids = ids.slice(0, 1);
    }
    const expandedSet = new Set(ids);
    for (const { itemId, itemEl, headerEl, bodyEl } of itemEls) {
      const expanded = expandedSet.has(itemId);
      headerEl.setAttribute('aria-expanded', expanded ? 'true' : 'false');
      if (expanded) {
        bodyEl.removeAttribute('hidden');
        itemEl.classList.add('tf-accordion__item--expanded');
      } else {
        bodyEl.setAttribute('hidden', '');
        itemEl.classList.remove('tf-accordion__item--expanded');
      }
    }
  };
  applyExpanded();
  const off = subscribeBindRef(expandedIdsBind, ctx.store, applyExpanded);
  ctx.registerCleanup(off);

  return wrapper;
}

// =============================================================================
// Pomocnik: bind text content do BindRef
// =============================================================================

function bindTextContent(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  const off = subscribeBindRef(bindRef, ctx.store, apply);
  ctx.registerCleanup(off);
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerLayoutCardsRenderers() {
  if (!lookupComponentRenderer(CARD_TAG)) registerComponentRenderer(CARD_TAG, renderCard);
  if (!lookupComponentRenderer(SECTION_CARD_TAG)) registerComponentRenderer(SECTION_CARD_TAG, renderSectionCard);
  if (!lookupComponentRenderer(COLLAPSIBLE_TAG)) registerComponentRenderer(COLLAPSIBLE_TAG, renderCollapsible);
  if (!lookupComponentRenderer(ACCORDION_TAG)) registerComponentRenderer(ACCORDION_TAG, renderAccordion);
}
