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
    { name: 'direct-http', testMatch: 'services-direct-http.spec.js' },
    { name: 'mesh-pairing', testMatch: 'mesh-pairing.spec.js' },
    { name: 'm16-aliases', testMatch: 'm16-services-aliases.spec.js' },
    { name: 'm14-bindings', testMatch: 'm14-bindings.spec.js' },
    { name: 'm15-wizard', testMatch: 'm15-install-wizard.spec.js' },
    { name: 'f1c-addon-ui', testMatch: 'addon-ui-iframe.spec.js' },
    { name: 'sdk-showcase-panel', testMatch: 'sdk-showcase-panel.spec.js' },
    { name: 'sdk-showcase-components', testMatch: 'sdk-showcase-components.spec.js' },
    { name: 'tentavision-panel', testMatch: 'tentavision-panel.spec.js' },
    { name: 'tentavision-cameras', testMatch: 'tentavision-cameras.spec.js' },
    { name: 'tentavision-camera-flow', testMatch: 'tentavision-camera-flow.spec.js' },
    { name: 'tentavision-real-camera', testMatch: 'tentavision-real-camera.spec.js' },
    { name: 'tentavision-dashboard', testMatch: 'tentavision-dashboard.spec.js' },
    { name: 'tentavision-profiles', testMatch: 'tentavision-profiles.spec.js' },
    { name: 'tentavision-alarms', testMatch: 'tentavision-alarms.spec.js' },
    { name: 'tentavision-search', testMatch: 'tentavision-search.spec.js' },
    { name: 'tentavision-audit', testMatch: 'tentavision-audit.spec.js' },
    { name: 'tentavision-settings', testMatch: 'tentavision-settings.spec.js' },
    { name: 'tentavision-models', testMatch: 'tentavision-models.spec.js' },
    { name: 'tentavision-zones', testMatch: 'tentavision-zones.spec.js' },
    { name: 'tentavision-live', testMatch: 'tentavision-live.spec.js' },
    { name: 'tentavision-live-stream', testMatch: 'tentavision-live-stream.spec.js' },
    { name: 'tentavision-reid', testMatch: 'tentavision-reid.spec.js' },
    { name: 'tentavision-bindings', testMatch: 'tentavision-bindings.spec.js' },
    { name: 'tentavision-onboarding', testMatch: 'tentavision-onboarding.spec.js' },
    { name: 'tentavision-evidence', testMatch: 'tentavision-evidence.spec.js' },
  ],
});
