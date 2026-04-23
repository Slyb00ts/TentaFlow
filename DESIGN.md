# TentaFlow Design System

Single source of truth dla wszystkich design decisions. Każdy nowy UI touchpoint **MUSI** używać tych tokens — zero hardcoded hex colors, zero magic numbers.

**Source:** extracted z `tentaflow-core/www/css/variables.css` + `components.css` + patterns z v3 mockups (`~/.gstack/projects/Slyb00ts-TentaFlow/designs/wireframes-20260417/`).

**Last updated:** 2026-04-17 · Week 0 Lane C deliverable (Task #23)

---

## 1. Design Philosophy

**Dark-first.** Light theme planowany w przyszłości (token aliasy przygotowane). Obecnie wszystkie wartości są tuned dla dark UI.

**Mascot-anchored branding.** Octopus z `tentaflow.png` pojawia się jako:
- Sidebar logo (30px width)
- Hero elements (120-140px)
- Chat AI avatar (30px padded w avatar circle)
- Error screens (110px z gentle-float animation)
- Feedback callouts (60-70px)

**Indigo primary / lilac accent.** Zero pink, magenta, purple gradients. Paleta matchuje scrum mockup (kolor semantic orbs różnobarwny ale primary UI jest indigo).

**SVG icons only.** Zero emoji w UI. Dowolna ikona = inline SVG z `<use href="#i-*">` sprite. Stroke weight 1.75, linecap/join round.

**Subtraction default** (Dieter Rams). Element nie earning pixeli → cut.

---

## 2. Color Tokens

### 2.1 Base palette (dark theme — primary)

```css
/* Backgrounds */
--color-bg-primary: #0f1117;      /* app background */
--color-bg-secondary: #1a1d27;    /* sidebar, topbar */
--color-bg-tertiary: #232733;     /* elevated panels (buttons secondary, stats) */
--color-bg-card: #1e2230;         /* card surfaces */
--color-bg-hover: #282d3e;        /* interactive hover state */
--color-bg-input: #161923;        /* form inputs, code blocks */

/* Text */
--color-text-primary: #e4e6ed;    /* body text */
--color-text-secondary: #8b8fa3;  /* muted labels */
--color-text-muted: #5c6078;      /* hint text, captions */
--color-text-inverse: #0f1117;    /* text on bright backgrounds (accent buttons) */
```

### 2.2 Accent + semantic

```css
/* Accent (indigo primary) */
--color-accent: #6366f1;           /* primary action color */
--color-accent-hover: #818cf8;     /* hover state */
--color-accent-light: rgba(99, 102, 241, 0.15);  /* backgrounds, subtle highlights */

/* Semantic states */
--color-success: #22c55e;          /* online, granted, saved */
--color-success-light: rgba(34, 197, 94, 0.15);
--color-warning: #f59e0b;          /* offline-grace, pending, beta badges */
--color-warning-light: rgba(245, 158, 11, 0.15);
--color-error: #ef4444;            /* errors, destructive actions */
--color-error-light: rgba(239, 68, 68, 0.15);
--color-error-hover: #dc2626;
--color-info: #3b82f6;             /* info, tailscale remote, links */
--color-info-light: rgba(59, 130, 246, 0.15);
```

### 2.3 Borders

```css
--color-border: #2a2e3f;           /* default border */
--color-border-hover: #3d4259;     /* hover state border */
```

### 2.4 Lilac accent (v2 extension, po mockup alignment)

Dla hero gradients + branding text (TENTAFLOW logo gradient). Nie dla UI chrome.

```css
--color-accent-2: #a78bfa;         /* lilac soft — secondary brand accent */
--gradient-logo: linear-gradient(90deg, #ffffff 0%, #60a5fa 50%, #a78bfa 100%);
--gradient-accent: linear-gradient(135deg, #6366f1 0%, #a78bfa 100%);
```

### 2.5 Rule: zero hardcoded hex

Każdy kolor w nowym kodzie używa CSS variable. Nigdy inline `color: #6366f1`. CI grep gate reject hex colors poza `variables.css`.

---

## 3. Typography

### 3.1 Font family

```css
--font-family: 'Manrope', -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
```

Manrope jest primary, system fallback dla edge cases. Font weight imported: 400 (body), 500 (medium emphasis), 600 (buttons, labels), 700 (headings), 800 (logo, hero titles).

### 3.2 Size scale

```css
--font-size-xs:  0.75rem;    /* 12px — captions, table headers */
--font-size-sm:  0.8125rem;  /* 13px — secondary text, small buttons */
--font-size-md:  0.875rem;   /* 14px — body, default buttons */
--font-size-lg:  1rem;       /* 16px — card titles, section headers */
--font-size-xl:  1.25rem;    /* 20px — page titles */
--font-size-2xl: 1.5rem;     /* 24px — hero subtitles */
--font-size-3xl: 2rem;       /* 32px — hero titles */
```

### 3.3 Weight pairings

- Body text: `md` 14px, weight 400
- Button labels: `md` 14px, weight 600
- Nav items: `sm` 13px, weight 500
- Section headers (UPPERCASE + letter-spacing 0.05em): `xs` 12px, weight 700
- Page title `h1`: `xl` 20px, weight 700
- Hero title: `3xl` 32px+, weight 800, gradient text fill

### 3.4 Line heights

- Body: 1.5 (comfortable reading)
- Headings: 1.2-1.3 (tight)
- Buttons/labels: 1.4 (balanced)

---

## 4. Spacing Scale

```css
--spacing-xs:  4px;    /* tight gaps between related inline elements */
--spacing-sm:  8px;    /* default gap (list items, chip groups) */
--spacing-md:  16px;   /* card padding, button padding horizontal */
--spacing-lg:  24px;   /* section separation, card padding y */
--spacing-xl:  32px;   /* page margin, major section gaps */
--spacing-2xl: 48px;   /* hero vertical padding */
```

**Konvencja:** nigdy nie używaj arbitrary values (margin: 13px). Jeśli scale nie ma properie wartości, dodaj variable albo połącz istniejące (margin: calc(var(--spacing-md) + var(--spacing-xs))).

---

## 5. Border Radius

```css
--radius-sm:  6px;   /* badges, chips, pills, small inputs */
--radius-md:  8px;   /* buttons, inputs, small cards */
--radius-lg:  12px;  /* modals, large cards */
--radius-xl:  16px;  /* hero blocks, feature cards */
```

**Konvencja:** dobierz radius do gęstości element'u. Małe = tight radius. Duże = lg/xl. Zero `border-radius: 50%` poza avatars + circular icons.

---

## 6. Shadow Scale

```css
--shadow-sm: 0 1px 3px  rgba(0, 0, 0, 0.3);   /* subtle lift (cards at rest) */
--shadow-md: 0 4px 12px rgba(0, 0, 0, 0.4);   /* hover state, dropdowns */
--shadow-lg: 0 8px 24px rgba(0, 0, 0, 0.5);   /* modals, popups */
```

**Extended dla mockup v3 branding:**

```css
--glow-accent: 0 0 30px rgba(99, 102, 241, 0.3);   /* hero mascot, accent states */
--glow-success: 0 0 12px rgba(34, 197, 94, 0.4);   /* online indicators */
--glow-info: 0 0 12px rgba(59, 130, 246, 0.4);     /* tailscale indicators */
```

---

## 7. Motion / Transitions

```css
--transition-fast:   150ms ease;  /* color/bg changes, hover feedback */
--transition-normal: 250ms ease;  /* modal open, accordion expand */
```

**Extended:**

```css
--transition-slow: 300ms ease-out;  /* entrance animations */
--ease-out:        cubic-bezier(0.16, 1, 0.3, 1);  /* dramatic entrance */
--ease-in:         cubic-bezier(0.7, 0, 0.84, 0);  /* acceleration exit */
```

**Reduced motion respect:**

```css
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    animation-duration: 0.01ms !important;
    transition-duration: 0.01ms !important;
  }
}
```

---

## 8. Layout Tokens

### 8.1 Sidebar

```css
--sidebar-width: 240px;             /* expanded (desktop default) */
--sidebar-collapsed-width: 64px;    /* icons-only (future mobile/tablet) */
```

### 8.2 Topbar

```css
--topbar-height: 56px;
```

### 8.3 Content max width

```css
--content-max-width: 1440px;  /* Admin screens max width */
--content-narrow:    800px;   /* Settings forms, single-column layouts */
```

---

## 9. Component Patterns

### 9.1 Buttons

Hierarchia: `primary` (indigo filled) > `secondary` (border) > `ghost` (transparent) > `danger` (red).

Sizes: default (md padding), `btn-sm` (xs padding).

```html
<button class="btn btn-primary">Primary</button>
<button class="btn btn-secondary">Secondary</button>
<button class="btn btn-ghost btn-sm">Ghost small</button>
<button class="btn btn-danger">Delete</button>
```

Focus visible: `outline: 2px solid var(--color-accent); outline-offset: 2px;` (keyboard nav).

### 9.2 Cards

```html
<div class="card">
  <div class="card-header"><h3>Title</h3></div>
  <div class="card-body">Content</div>
</div>
```

Default: `bg-card`, `shadow-sm`, `radius-lg`, border `1px solid var(--color-border)`.

### 9.3 Tables

Always z `.table-wrapper` dla horizontal scroll. Header: uppercase + letter-spacing 0.05em. Row hover: `bg-hover`. Zero alternating stripes (konflikt z hover highlight).

### 9.4 Forms

```html
<div class="form-group">
  <label>Label</label>
  <input type="text" class="form-input">
  <div class="form-help">Help text</div>
  <div class="form-error">Error message (when invalid)</div>
</div>
```

Input focus: border-color var(--color-accent).

### 9.5 Modals

Overlay: `rgba(0, 0, 0, 0.8)` + backdrop-filter blur(8px). Modal: `bg-card`, `radius-lg`, `shadow-lg`, max-width 480px.

Close: `Esc` key, click overlay, explicit `.modal-close` button top-right.

### 9.6 Badges / Chips

- **Status chip** (online/offline/pending): `font-size-xs`, `radius-sm`, padding 2px 8px, bg = `*-light` semantic color, fg = semantic color solid.
- **Scope chip** (API key permissions, tags): color-coded per category (chat blue, deploy green, mesh-admin indigo, trace amber, license cyan).
- **Count badge** (sidebar item count): `bg-accent-light`, `color-accent`, radius 8px.

### 9.7 Gauge (circular progress)

Dla CPU/RAM/VRAM/Load metrics. 64×64px z conic-gradient ring.

```html
<div class="gauge">
  <div class="gauge-ring" style="--pct: 42;">
    <div class="gauge-val">42<span>%</span></div>
  </div>
  <div class="gauge-label">CPU</div>
  <div class="gauge-sub">Intel i7 · 16 cores</div>
</div>
```

Color: default accent, `.hot` class triggers warning color gdy >85%.

### 9.8 Sparkline (inline mini-chart)

Reuse istniejący `www/js/modules/mesh/sparkline-chart.js`. SVG inline, 60×20px typical. Używaj dla time-series w stat cards (tokens/s over last 60s, requests per minute).

### 9.9 Empty states

Każda lista/tabela ma empty state. NIE "No items." Zamiast:

- Icon/illustration (mascot opcjonalnie)
- Friendly copy ("No API keys yet. Create your first key to enable programmatic access.")
- Primary CTA button

### 9.10 Toast notifications

Top-right, auto-dismiss 4s. Kolorystyka per severity. Stacked gdy multiple. Destructive actions: 10s undo window.

---

## 10. SVG Icon Standards

### 10.1 Source

Inline SVG sprite. Zero icon fonts, zero emoji, zero external CDN.

Definition pattern:

```html
<svg width="0" height="0" style="position:absolute" aria-hidden="true">
  <defs>
    <symbol id="i-home" viewBox="0 0 24 24">
      <path d="M3 10.5 12 3l9 7.5"/>
      <path d="M5 9.5V20a1 1 0 0 0 1 1h4v-6h4v6h4a1 1 0 0 0 1-1V9.5"/>
    </symbol>
    <!-- ... ~60 symbols total -->
  </defs>
</svg>
```

Usage:

```html
<svg class="icon"><use href="#i-home"/></svg>
```

### 10.2 Sizes

```css
.icon     { width: 16px; height: 16px; stroke-width: 1.75; stroke: currentColor; fill: none; stroke-linecap: round; stroke-linejoin: round; flex-shrink: 0; }
.icon-lg  { width: 20px; height: 20px; }
.icon-xl  { width: 24px; height: 24px; }
```

### 10.3 Style guide

- Stroke-based (nie filled) dla consistency z Lucide family
- Stroke width 1.75 baseline, 2 dla small icons (14px), 1.5 dla large icons (32px+)
- `stroke-linecap: round` + `stroke-linejoin: round` (soft corners)
- `currentColor` stroke (odziedzicza text color of parent)
- ViewBox 24×24 standard

### 10.4 Rule: NEVER emoji w UI

Commit 18ccfce explicite replaced emoji z SVG. User-stated HARD RULE w learnings. Grep CI gate: reject emoji characters w www/**/*.{html,js} poza i18n string values.

---

## 11. Accessibility Requirements

### 11.1 Focus outline

```css
*:focus-visible {
  outline: 2px solid var(--color-accent);
  outline-offset: 2px;
}
```

Never `outline: none` bez replacement focus indicator.

### 11.2 Touch targets

Minimum 44×44px dla wszystkich interactive elementów (buttons, links, toggle). Na mobile critical, na desktop dobra higiena.

### 11.3 Color contrast

Minimum 4.5:1 dla body text (WCAG AA). Our palette verified:
- `text-primary` (#e4e6ed) na `bg-primary` (#0f1117) = **14.2:1** ✅
- `text-secondary` (#8b8fa3) na `bg-primary` = **5.8:1** ✅
- `text-muted` (#5c6078) na `bg-primary` = **3.2:1** ⚠️ (tylko dla decorative, nie body)

Action buttons text (white on accent): #ffffff na #6366f1 = **4.7:1** ✅

### 11.4 Keyboard navigation

- All interactive elements tab-reachable
- Modal focus trap (Tab/Shift+Tab cycle within modal)
- Esc closes modals/dropdowns
- Arrow keys navigate lists/trees (tree widget roving tabindex)
- Enter/Space activate buttons

### 11.5 Screen readers

- Landmarks: `<main>`, `<nav>`, `<aside>`, `<header>`, `<footer>`
- Form labels explicit (`<label for="id">` nie placeholder-only)
- Button `aria-label` dla icon-only buttons
- Live regions (`role="alert"`, `aria-live="polite"`) dla toasts, error screen, status updates
- Table `<thead>` + `<tbody>` semantic

### 11.6 Reduced motion

```css
@media (prefers-reduced-motion: reduce) {
  /* Disable all animations + transitions */
}
```

### 11.7 Language declaration

```html
<html lang="pl">
<!-- for sub-content: -->
<span lang="en">English text</span>
```

---

## 12. Responsive Breakpoints

```css
/* Mobile first approach */
/* Default: mobile (<768px) */

@media (min-width: 768px) {
  /* Tablet */
}

@media (min-width: 1024px) {
  /* Desktop */
}

@media (min-width: 1440px) {
  /* Wide desktop */
}
```

### 12.1 Per screen strategy

- **Admin screens** (dashboard, settings, users, audit, rules): `≥1024px` preferred. `<1024px`: sidebar collapsible z hamburger, table overflow horizontal.
- **User apps** (chat, obrazy, notes, meeting): full responsive, all viewports usable.
- **Dev tools** (trace/replay, flow builder): desktop-only. `<768px` wyświetla "Devtools wymaga viewport ≥768px" message.

### 12.2 Mobile sidebar

< 768px: sidebar zamyka się do hamburger icon w topbar. Open = slide-in overlay z close button.

---

## 13. Theme Preparation

Dark theme is default. Light theme planowany przez dodanie `[data-theme="light"]` overrides:

```css
[data-theme="light"] {
  --color-bg-primary: #ffffff;
  --color-bg-secondary: #f6f7f9;
  /* ...etc */
}
```

User toggle w Settings → preference persisted w localStorage + server-side user profile.

System preference detect:

```css
@media (prefers-color-scheme: light) { /* if no user override, use system */ }
```

---

## 14. Mockup Reference

Visual anchor: mockupy v3 w `~/.gstack/projects/Slyb00ts-TentaFlow/designs/wireframes-20260417/index.html`. 25 screens z actual HTML rendering używające powyższych tokens. Implementator czyta mockup dla WHAT-LOOKS-LIKE, ten dokument dla WHICH-TOKEN-TO-USE.

Feature-preservation details: `FEATURES-TO-PRESERVE.md` w tym samym folderze.

---

## 15. Maintenance

**Source of truth hierarchy:**
1. `www/css/variables.css` — authoritative token values
2. Ten dokument — documentation + patterns + rules
3. Mockupy — visual reference

**Gdy dodajesz nowy token:** edytuj `variables.css` FIRST, potem update tego dokumentu. Nigdy duplikuj wartości w ten dokument bez referencji.

**Gdy dodajesz nowy komponent:** pisz w `www/css/components.css` używając istniejących tokens. Sekcję patterns w tym docu update if pattern is reusable.

**CI gates associated:**
- Grep reject: hardcoded hex colors poza `variables.css`
- Grep reject: emoji characters w UI HTML/JS (poza i18n values)
- Grep reject: `outline: none` bez replacement focus style
- Grep enforce: `lang=` attribute w HTML root

---

## 16. Version History

- **v1** (2026-04-17) — initial extraction z variables.css + components.css + mockup v3 decisions. Extended tokens: accent-2 lilac, gradient-logo, glow-* shadows, ease-* cubic-beziers, content-max-width, content-narrow. 16 sekcji.

Bumps przy breaking token changes. Non-breaking extensions w pierwszej dekadzie minor increments.
