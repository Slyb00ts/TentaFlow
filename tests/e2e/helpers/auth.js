// =============================================================================
// File: tests/e2e/helpers/auth.js
// Description: Browser login helper for UI e2e tests. Drives the actual login
//              page via Playwright (so the JWT lands in localStorage/cookies
//              the same way a real user would have it).
// =============================================================================

const { baseUrl } = require('./spawn');

// A node that has never been logged into refuses every call but the password
// change (`user_accounts.must_change_password`, the migration default), so the
// login card is replaced in place by the rotation card and the shell never
// appears. The rotated value is remembered per node+user because one worker
// runs several spec files, each against its own freshly created node: reusing
// one file's rotation on another file's node would fail its first login.
const rotatedPasswords = new Map();

function rotationKey(port, username) {
  return `${port ?? 'default'}:${username}`;
}

// The core spends a per-username budget of 10 logins per 60 s
// (`auth::rate_limit`, "too many login attempts, retry in a minute"), and a
// spec file runs its tests sequentially in one worker against one node, so a
// suite with more than ten tests logs in faster than the product allows. The
// surplus login is refused, the shell never appears, and the failure looks
// like a screen that does not render. Spend the budget deliberately instead:
// hold a login until the window has room for it.
const LOGIN_WINDOW_MS = 60_000;
const MAX_LOGINS_PER_WINDOW = 10;
// The rotation signs in a second time after changing the password (that change
// is charged to a different bucket, `password:<user>`, so only the sign-in
// counts here), and whether a node needs rotating is known only after the
// first submit. Keep one attempt in hand so that second submit always fits.
const LOGIN_ATTEMPT_RESERVE = 1;
const loginAttempts = new Map();

function recordLoginAttempt(key) {
  const now = Date.now();
  const recent = (loginAttempts.get(key) ?? []).filter((t) => now - t < LOGIN_WINDOW_MS);
  recent.push(now);
  loginAttempts.set(key, recent);
}

async function awaitLoginBudget(key) {
  for (;;) {
    const now = Date.now();
    const recent = (loginAttempts.get(key) ?? []).filter((t) => now - t < LOGIN_WINDOW_MS);
    loginAttempts.set(key, recent);
    if (recent.length < MAX_LOGINS_PER_WINDOW - LOGIN_ATTEMPT_RESERVE) return;
    await new Promise((resolve) => setTimeout(resolve, LOGIN_WINDOW_MS - (now - recent[0]) + 250));
  }
}

async function loginAsAdmin(page, { username = 'admin', password = 'admin', port, rotateTo } = {}) {
  const url = port ? `https://127.0.0.1:${port}/` : `${baseUrl()}/`;
  const key = rotationKey(port, username);
  const current = rotatedPasswords.get(key) ?? password;
  await page.goto(url);

  // The SPA shows the login screen when no JWT is present. Wait for the
  // username field rendered by tf-input, then submit credentials.
  const userInput = page.locator('#login-username input').first();
  await userInput.waitFor({ state: 'visible', timeout: 15000 });
  await userInput.fill(username);

  const passInput = page.locator('#login-password input').first();
  await passInput.fill(current);

  // tf-button renders a real <button> in shadow DOM; click the host.
  await awaitLoginBudget(key);
  recordLoginAttempt(key);
  await page.locator('#login-submit').click();

  // After successful login the router replaces the login card with the main
  // shell — but on a node that still owes the initial rotation it is the
  // rotation card that appears. Race the two and walk the rotation, otherwise
  // a spec that has just created its node waits out the full timeout here.
  try {
    await Promise.race([
      page.waitForSelector('aside, nav, [data-screen], #main, #app-shell', { timeout: 30000 }),
      page.waitForSelector('#login-new-password input', { timeout: 30000 }),
    ]);
  } catch (err) {
    // Neither the shell nor the rotation card means the login was refused. The
    // card states why (`#login-error` carries the protocol message verbatim),
    // and swallowing that turns a specific refusal into a bare timeout.
    const refusal = await page
      .locator('#login-error')
      .textContent()
      .catch(() => null);
    throw new Error(
      `login as ${username} reached neither the app shell nor the password rotation card; ` +
        `login screen said: ${refusal?.trim() || '(no message)'}`,
      { cause: err },
    );
  }
  if (await page.locator('#login-new-password input').count() === 0) return;

  // The server refuses a new password equal to the current one
  // (`auth_password_change`), so the rotation needs a different value; the
  // screen then signs in with it and lands on the shell. The card is a fresh
  // mount, so the current password must be typed again — the field filled
  // before the rotation was a different, detached element.
  const next = rotateTo ?? `${current}-rotated`;
  await page.locator('#login-password input').first().fill(current);
  await page.locator('#login-new-password input').first().fill(next);
  await page.locator('#login-confirm-password input').first().fill(next);
  await page.locator('#login-submit').click();
  await page.waitForSelector('aside, nav, [data-screen], #main, #app-shell', { timeout: 30000 });
  recordLoginAttempt(key);
  rotatedPasswords.set(key, next);
}

module.exports = { loginAsAdmin };
