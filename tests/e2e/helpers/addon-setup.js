// =============================================================================
// File: tests/e2e/helpers/addon-setup.js
// Description: Helpers for addon-panel e2e specs. Installs an addon instance
//              from the bundled package catalog over the binary protocol
//              (same ApiBinary path the dashboard uses), grants permission
//              defaults, enables the instance, and collects browser console
//              errors for assertion.
// =============================================================================

/**
 * Installs an addon instance from the catalog, grants the given permissions
 * as addon defaults (allow) and enables the instance. Must be called on a
 * page that is already logged in as admin (binary WS authenticated).
 *
 * Returns the synthetic instance addon_id (e.g. "sdk-showcase-1a2b3c4d").
 */
async function installAddonInstance(page, { packageId, displayName, permissions }) {
  return await page.evaluate(async ({ packageId, displayName, permissions }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');

    const packages = await ApiBinary.list('addonCatalogListRequest', { arrayKey: 'packages' });
    const pkg = packages.find((p) => p.packageId === packageId);
    if (!pkg) {
      throw new Error(`package '${packageId}' not found in catalog: ${packages.map((p) => p.packageId).join(', ')}`);
    }
    const version = pkg.latestVersion ?? pkg.versions[0];

    const res = await ApiBinary.action('addonInstanceInstallRequest', {
      packageId,
      version,
      displayName,
    });
    if (!res.ok) throw new Error(`install failed: ${res.error}`);
    const addonId = res.addonId;

    for (const permissionId of permissions) {
      await ApiBinary.action('addonPermissionDefaultSetRequest', {
        addonId,
        permissionId,
        grantMode: 'allow',
      });
    }

    const toggled = await ApiBinary.action('addonToggleRequest', { addonId, enabled: true });
    if (toggled.ok === false) throw new Error(`enable failed: ${toggled.error}`);

    return addonId;
  }, { packageId, displayName, permissions });
}

/**
 * Attaches console-error and pageerror collectors to a page. Returns the
 * shared array; entries are prefixed with the source kind.
 */
// Environmental noise only — NOT app bugs. The test instance runs on a
// self-signed cert; the service-worker fetch ignores Playwright's
// ignoreHTTPSErrors and logs an SSL error on registration.
const IGNORED_CONSOLE = [
  /An SSL certificate error occurred when fetching the script/,
];

function collectConsoleErrors(page) {
  const errors = [];
  // Ring buffer of ALL console output — attached to failure diagnostics so
  // flaky runs carry the full client-side trace. Non-enumerable so
  // `expect(errors).toEqual([])` still treats the array as plain.
  Object.defineProperty(errors, 'fullLog', { value: [], enumerable: false });
  page.on('console', (msg) => {
    const text = msg.text();
    errors.fullLog.push(`[${msg.type()}] ${text}`);
    if (errors.fullLog.length > 120) errors.fullLog.splice(0, errors.fullLog.length - 120);
    if (msg.type() !== 'error') return;
    if (IGNORED_CONSOLE.some((re) => re.test(text))) return;
    errors.push(`[console.error] ${text}`);
  });
  page.on('pageerror', (err) => {
    errors.push(`[pageerror] ${err.message}`);
  });
  return errors;
}

/**
 * Formats collected console errors plus the backend log tail for a failure
 * message. `proc` is the child returned by startBinary (carries logTail).
 */
function diagnostics(errors, proc) {
  const backend = (proc?.logTail ?? []).slice(-60).join('');
  const clientLog = (errors.fullLog ?? []).slice(-60).join('\n');
  return `console errors:\n${errors.join('\n')}\n--- client console tail ---\n${clientLog}\n--- backend log tail ---\n${backend}`;
}

module.exports = { installAddonInstance, collectConsoleErrors, diagnostics };
