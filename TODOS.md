# TODOS

## Design Debt

### Utworz DESIGN.md — formalny design system
- **What:** Utworz DESIGN.md dokumentujacy design system projektu: tokeny (z variables.css), wzorce komponentow (karty, gauge, modale, sparklines), styl ikon SVG, wytyczne a11y.
- **Why:** Bez formalnego design system kazdy developer zgaduje styl. variables.css definiuje tokeny ale nie dokumentuje kiedy i jak ich uzywac.
- **Pros:** Spojnosc wizualna, szybsze onboardowanie, mniej review comments o stylu.
- **Cons:** Wymaga utrzymania dokumentu przy zmianach.
- **Context:** Decyzja z /plan-design-review 2026-03-31. Plan node detail dashboard redesign dodal wiele nowych decyzji (sparkline spec, SVG icons, a11y labels) ktore powinny byc w jednym miejscu.
- **Depends on:** Nic. Mozna uruchomic `/design-consultation` w dowolnym momencie.
