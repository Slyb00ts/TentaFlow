# TentaFlow Design System

System projektowy TentaFlow mieszka w katalogu **[`design/`](design/README.md)**.

- Tokeny (kolory, typografia, odstępy, promienie, cienie, ruch, breakpointy):
  [`design/tokens/tokens.json`](design/tokens/tokens.json) — jedyne źródło wartości.
- Fundamenty: [`design/foundations/`](design/foundations/) · kontrolki:
  [`design/components/`](design/components/README.md) · wzorce stron:
  [`design/patterns/`](design/patterns/) · platformy: [`design/platforms/`](design/platforms/).
- Jak dodać stronę lub komponent, zasady i governance: [`design/README.md`](design/README.md).
- Rozjazd między wersją 1.0 tego dokumentu (2026-04-17) a kodem oraz plan
  prostowania starego CSS: [`design/MIGRATION.md`](design/MIGRATION.md).
- Natywny silnik UI (TentaEngine) i jego wpięcie:
  [`docs/TENTAENGINE_INTEGRATION_PLAN.md`](docs/TENTAENGINE_INTEGRATION_PLAN.md).

Wersja 1.0 opisywała słownik tokenów (`--color-bg-primary`, `--spacing-md`,
`variables.css`), którego nigdy nie było w kodzie; jej treść została zastąpiona
wersją 2.0 (2026-09-14) opartą na wartościach z żywych arkuszy `style.css` i
`controls.css` oraz na enumach `tentaflow-sdk-spec::protocol::ui::tokens`.
