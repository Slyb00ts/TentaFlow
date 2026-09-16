// =============================================================================
// Plik: tentanas.playwright.config.js
// Opis: Izolowana konfiguracja testów UI TentaNas, bez uruchamiania backendu.
// Przykład: npx playwright test --config=tentanas.playwright.config.js
// =============================================================================

module.exports = {
  testDir: '.',
  testMatch: ['tentanas-targets.spec.js', 'tentanas-environment.spec.js', 'tentanas-snapshots.spec.js', 'tentanas-elastic-ui.spec.js'],
  timeout: 30000,
  workers: 1,
  reporter: 'list',
  use: {
    launchOptions: process.env.TENTANAS_E2E_BROWSER ? { executablePath: process.env.TENTANAS_E2E_BROWSER } : {},
  },
  outputDir: process.env.TENTANAS_E2E_ARTIFACTS || 'test-results/tentanas',
};
