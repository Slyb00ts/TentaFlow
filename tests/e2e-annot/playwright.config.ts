// =============================================================================
// Plik: tests/e2e-annot/playwright.config.ts
// Opis: Konfiguracja Playwright dla testu E2E edytora anotacji ML Studio.
//       Dashboard na HTTPS z self-signed cert → ignoreHTTPSErrors.
// =============================================================================

import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './specs',
  timeout: 120000,
  expect: { timeout: 15000 },
  retries: 0,
  workers: 1,
  reporter: [['list']],
  use: {
    baseURL: 'https://localhost:8095',
    ignoreHTTPSErrors: true,
    screenshot: 'only-on-failure',
    trace: 'retain-on-failure',
    actionTimeout: 20000,
  },
  projects: [{ name: 'chromium', use: { browserName: 'chromium' } }],
});
