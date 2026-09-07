// =============================================================================
// Plik: tentanas-elastic-live.playwright.config.js
// Opis: Odbiór istniejącego API VM przez lokalny tunel HTTPS, bez uruchamiania VM.
// Przykład: npx playwright test --config=tentanas-elastic-live.playwright.config.js
// =============================================================================

const { defineConfig } = require('@playwright/test');

const baseURL = process.env.TENTANAS_LIVE_BASE_URL;
if (!baseURL || !/^https:\/\/127\.0\.0\.1:[1-9][0-9]{0,4}\/?$/.test(baseURL)) {
  throw new Error('TENTANAS_LIVE_BASE_URL musi wskazywać dokładnie https://127.0.0.1:<port>');
}
const endpoint = new URL(baseURL);
if (Number(endpoint.port) > 65535) throw new Error('Nieprawidłowy port lokalnego tunelu');

module.exports = defineConfig({
  testDir: '.',
  testMatch: 'tentanas-elastic-live.spec.js',
  timeout: 120000,
  workers: 1,
  retries: 0,
  reporter: 'list',
  outputDir: process.env.TENTANAS_LIVE_ARTIFACTS || 'test-results/tentanas-elastic-live',
  use: {
    baseURL,
    ignoreHTTPSErrors: true,
    viewport: { width: 1440, height: 1000 },
    actionTimeout: 15000,
    trace: 'off',
    video: 'off',
    screenshot: 'off',
    launchOptions: process.env.TENTANAS_E2E_BROWSER
      ? { executablePath: process.env.TENTANAS_E2E_BROWSER } : {},
  },
});
