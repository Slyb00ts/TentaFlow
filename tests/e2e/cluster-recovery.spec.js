// @ts-check
const { test, expect } = require('@playwright/test');

const BASE_URL = process.env.TENTAFLOW_TEST_URL || 'https://192.168.11.24:8090';

test.describe('Cluster Model Auto-Recovery E2E', () => {
  test.beforeEach(async ({ page }) => {
    page.on('pageerror', (err) => console.log('[pageerror]', err.message));
  });

  test('Cluster deployment is active and visible in UI', async ({ page }) => {
    // 1. Open login page
    await page.goto(BASE_URL + '/');
    await page.waitForTimeout(1000);

    // If login is required:
    const userInput = page.locator('#login-username input').first();
    if (await userInput.isVisible({ timeout: 5000 }).catch(() => false)) {
      await userInput.fill('admin');
      const passInput = page.locator('#login-password input').first();
      await passInput.fill('admin');
      await page.locator('#login-submit').click();
      await page.waitForSelector('#app-shell, aside, nav, [data-screen], #main', { timeout: 30000 });
      await page.waitForTimeout(1000);
    }

    // 2. Navigate to Clusters view via sidebar or URL
    const clusterNav = page.locator('.sidebar [data-view=clusters], a[href=#clusters], [data-view=clusters]').first();
    if (await clusterNav.isVisible({ timeout: 5000 }).catch(() => false)) {
      await clusterNav.click();
    } else {
      await page.goto(BASE_URL + '/#clusters');
    }

    await page.waitForSelector('.clusters-shell, #clusters-page-header, [data-cluster-detail]', { timeout: 20000 });

    // Wait for cluster cards
    const clusterCard = page.locator('[data-cluster-detail]').first();
    await expect(clusterCard).toBeVisible({ timeout: 15000 });

    const clusterName = await clusterCard.locator('.cluster-card-title, h3, h2, strong').first().textContent();
    console.log('Found cluster card:', clusterName);
    expect(clusterName).toContain('Cluster');

    // 3. Click cluster card to open ClusterDetailScreen
    await clusterCard.click();
    await page.waitForSelector('.cluster-detail', { timeout: 15000 });

    // 4. Verify deployment section
    const deploySection = page.locator('.cluster-deploy-section');
    await expect(deploySection).toBeVisible({ timeout: 10000 });

    // Verify active deployment exists
    const activeDeploy = page.locator('.cluster-deploy-active');
    await expect(activeDeploy).toBeVisible({ timeout: 15000 });

    // Check status chip inside active deployment
    const statusChip = activeDeploy.locator('tf-chip').first();
    await expect(statusChip).toBeVisible();
    const statusText = await statusChip.textContent();
    console.log('Deployment status chip text:', statusText);
    expect(statusText).toContain('Deployment active');

    // Verify model name and engine in deployment details
    const activeText = await activeDeploy.textContent();
    console.log('Active deployment summary:', activeText);
    expect(activeText).toContain('GLM-5.3-Flash-NVFP4');
    expect(activeText).toContain('sglang-glm53');
    expect(activeText).toContain('spark-001');
    expect(activeText).toContain('spark-002');

    // 5. Navigate to Services view via sidebar
    const servicesNav = page.locator('.sidebar [data-view=services], a[href=#services], [data-view=services]').first();
    if (await servicesNav.isVisible({ timeout: 5000 }).catch(() => false)) {
      await servicesNav.click();
    } else {
      await page.goto(BASE_URL + '/#services');
    }

    await page.waitForSelector('#services-page-header, .services-table, .services-list, #content, .empty-state', { timeout: 20000 });
    await page.waitForTimeout(2000);

    const pageContent = await page.content();
    expect(pageContent).toContain('GLM-5.3-Flash');
    console.log('Verified GLM-5.3-Flash in Services view');
  });
});
