// =============================================================================
// File: tests/e2e/helpers/ml-studio.js
// Description: Drives the ML Studio create wizard (name/description -> type ->
//              data -> summary) for the standalone ML Studio scripts, so the
//              wizard's step order lives in one place.
// =============================================================================

// Creates a project from the list screen and waits for its detail page. A type
// slug that the node does not offer leaves the wizard's default (first) type.
// Resolves to the new project's id, read from the detail route.
async function createProjectViaWizard(page, { name, description, type }) {
  await page.locator('#ml-studio-new').click();
  const nameInput = page.locator('#ml-studio-wiz-name input').first();
  await nameInput.waitFor({ state: 'visible', timeout: 10000 });
  await nameInput.fill(name);
  await page.locator('#ml-studio-wiz-desc textarea').first().fill(description);
  await page.locator('#ml-studio-wiz-next').click();

  const radio = page.locator(`#ml-studio-wiz-types tf-radio[value="${type}"]`).first();
  await radio.waitFor({ state: 'attached', timeout: 10000 });
  await radio.click();
  await page.locator('#ml-studio-wiz-next').click();

  // The data step is optional; the file is added later in the Dane tab.
  await page.locator('#ml-studio-wiz-next').click();
  await page.locator('#ml-studio-wiz-create').click();
  await page.waitForSelector('#ml-studio-detail tf-detail-header', { timeout: 15000 });
  return new URLSearchParams(new URL(page.url()).hash.split('?')[1] || '').get('projectId');
}

module.exports = { createProjectViaWizard };
