// =============================================================================
// File: tests/e2e/helpers/auth.js
// Description: Browser login helper for UI e2e tests. Drives the actual login
//              page via Playwright (so the JWT lands in localStorage/cookies
//              the same way a real user would have it).
// =============================================================================

const { baseUrl } = require('./spawn');

async function loginAsAdmin(page, { username = 'admin', password = 'admin', port } = {}) {
  const url = port ? `https://127.0.0.1:${port}/` : `${baseUrl()}/`;
  await page.goto(url);

  // The SPA shows the login screen when no JWT is present. Wait for the
  // username field rendered by tf-input, then submit credentials.
  const userInput = page.locator('#login-username input').first();
  await userInput.waitFor({ state: 'visible', timeout: 15000 });
  await userInput.fill(username);

  const passInput = page.locator('#login-password input').first();
  await passInput.fill(password);

  // tf-button renders a real <button> in shadow DOM; click the host.
  await page.locator('#login-submit').click();

  // After successful login the router replaces the login card with the main
  // shell — wait for a sidebar element to appear.
  await page.waitForSelector('aside, nav, [data-screen], #main, #app-shell', { timeout: 15000 });
}

module.exports = { loginAsAdmin };
