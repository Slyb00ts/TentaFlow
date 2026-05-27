// =============================================================================
// File: tests/e2e/playwright.config.js
// Description: Playwright config — mesh-pairing tests plus M14/M15/M16 UI
//              e2e tests. Each suite runs as a separate project so failures
//              are isolated and binary spawning does not conflict.
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
    { name: 'mesh-pairing', testMatch: 'mesh-pairing.spec.js' },
    { name: 'm16-aliases', testMatch: 'm16-services-aliases.spec.js' },
    { name: 'm14-bindings', testMatch: 'm14-bindings.spec.js' },
    { name: 'm15-wizard', testMatch: 'm15-install-wizard.spec.js' },
    { name: 'f1c-addon-ui', testMatch: 'addon-ui-iframe.spec.js' },
  ],
});
