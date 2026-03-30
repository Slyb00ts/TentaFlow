// =============================================================================
// Plik: tests/e2e/playwright.config.js
// Opis: Konfiguracja Playwright dla testow E2E mesh pairing.
// =============================================================================

const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: '.',
  testMatch: '*.spec.js',
  timeout: 120000,
  retries: 0,
  use: {
    headless: true,
    viewport: { width: 1280, height: 720 },
    ignoreHTTPSErrors: true,
  },
  projects: [
    {
      name: 'mesh-pairing',
      testMatch: 'mesh-pairing.spec.js',
    },
  ],
});
