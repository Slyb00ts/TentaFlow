// =============================================================================
// File: tests/e2e/robots-detail.spec.js
// Description: E2E for the robot detail screen (Roboty → robot). No physical
//              robot is needed: the page's robots protocol calls are replaced in
//              the browser (robots list, control, lidar/scene/depth streams), the
//              camera tile is fed a synthetic 16:9 canvas stream. Proves:
//              - camera + 3D tiles fill the free viewport height at any size and
//                follow a window resize;
//              - the whole camera frame stays visible (letterbox, no crop);
//              - LiDAR + camera-depth streams open by default;
//              - LiDAR off keeps the camera-depth stream, and the toggle does not
//                bounce back while polls still report the old value.
//              Screenshots → /tmp/robots-e2e/.
// =============================================================================

const fs = require('fs');
const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');

const BASE_PORT = 18421;
const SHOT_DIR = '/tmp/robots-e2e';
const ROBOT_ID = 'go2-e2e';

let PORT;
let proc;

test.beforeAll(async ({}, testInfo) => {
  test.skip(!binaryExists(), 'tentaflow binary not built (target_shared/{release-fast,release,debug})');
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  proc = startBinary({ port: PORT, db: `/tmp/e2e-robots-${PORT}.db` });
  await waitForServer(PORT);
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((r) => setTimeout(r, 1500));
});

// Replaces the robots protocol calls inside the page. `window.__robotsMock`
// holds the robot the server "reports", every control request and every
// stream subscription (with its open/closed state).
async function installRobotsMock(page) {
  await page.evaluate(async (robotId) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const mock = {
      robot: {
        robotId,
        kind: 'Unitree Go2',
        status: 'online',
        cameraId: 'cam-e2e',
        isLocal: true,
        ownerNodeId: 'local',
        batteryPercent: 81,
        rttMs: 24,
        capabilities: ['camera', 'lidar'],
        // Toggle kinds the robot advertises; the UI must keep them off the
        // generic control buttons (they have dedicated toggles).
        actionsMeta: [
          { kind: 'lidar_on', label: 'LiDAR włącz', risk: 'low', params: [] },
          { kind: 'lidar_off', label: 'LiDAR wyłącz', risk: 'low', params: [] },
          { kind: 'gestures_on', label: 'Gesty włącz', risk: 'low', params: [] },
          { kind: 'gestures_off', label: 'Gesty wyłącz', risk: 'low', params: [] },
        ],
        telemetry: null,
        lidar: { enabled: true, available: true, pointCount: 1200 },
        gesturesEnabled: false,
      },
      controls: [],
      subs: [],
      controlDelayMs: 300,
    };
    window.__robotsMock = mock;
    const list = ApiBinary.list.bind(ApiBinary);
    const action = ApiBinary.action.bind(ApiBinary);
    const subscribe = ApiBinary.subscribe.bind(ApiBinary);
    ApiBinary.list = async (kind, opts) => {
      if (kind === 'robotsListRequest') return [structuredClone(mock.robot)];
      return list(kind, opts);
    };
    ApiBinary.action = async (kind, payload, opts) => {
      if (kind === 'robotControlRequest') {
        mock.controls.push(payload.kind);
        await new Promise((r) => setTimeout(r, mock.controlDelayMs));
        return { ok: true };
      }
      if (kind === 'robotGeoAnchorGetRequest' || kind === 'robotGeoAnchorSetRequest') {
        return { ok: false, error: 'mock' };
      }
      return action(kind, payload, opts);
    };
    ApiBinary.subscribe = async (kind, payload, handlers) => {
      const id = payload?.streamId ?? '';
      if (/^(lidar|scene|scene-depth|camera|detections):/.test(id) || id.startsWith('vision')) {
        const rec = { id, open: true };
        mock.subs.push(rec);
        return () => { rec.open = false; };
      }
      return subscribe(kind, payload, handlers);
    };
  }, ROBOT_ID);
}

async function openRobotDetail(page) {
  await installRobotsMock(page);
  await page.evaluate(() => { window.location.hash = '#/robots'; });
  const open = page.locator(`[data-robot-card="${ROBOT_ID}"] [data-open-detail]`).first();
  await expect(open).toBeVisible({ timeout: 15000 });
  await open.click();
  await expect(page.locator('#robots-detail')).toBeVisible({ timeout: 10000 });
  await expect(page.locator('[data-field="camera-tile"] tf-video-stream')).toBeVisible({ timeout: 10000 });
}

// Feeds the camera tile a synthetic 16:9 frame (a framed test card) instead of
// the robot's MSE stream, so the fit of the whole frame can be measured.
async function feedSyntheticCamera(page) {
  await page.evaluate(() => {
    const tile = document.querySelector('[data-field="camera-tile"] tf-video-stream');
    const video = tile.shadowRoot.querySelector('video');
    const c = document.createElement('canvas');
    c.width = 1280;
    c.height = 720;
    const g = c.getContext('2d');
    const draw = () => {
      g.fillStyle = '#2f6bff';
      g.fillRect(0, 0, c.width, c.height);
      g.strokeStyle = '#ffd60d';
      g.lineWidth = 24;
      g.strokeRect(12, 12, c.width - 24, c.height - 24);
      g.fillStyle = '#fff';
      g.font = 'bold 72px sans-serif';
      g.fillText('16:9 FRAME', 420, 380);
    };
    draw();
    window.__camTimer = setInterval(draw, 100);
    video.src = '';
    video.srcObject = c.captureStream(10);
    video.play().catch(() => {});
    const status = tile.shadowRoot.querySelector('.status');
    if (status) status.hidden = true;
  });
  await page.waitForFunction(() => {
    const v = document.querySelector('[data-field="camera-tile"] tf-video-stream')
      ?.shadowRoot.querySelector('video');
    return v && v.videoWidth > 0;
  }, null, { timeout: 10000 });
}

// The rectangle the decoded frame actually occupies inside the <video> box, per
// its object-fit, plus the box itself (CSS pixels).
async function cameraFrameGeometry(page) {
  return page.evaluate(() => {
    const v = document.querySelector('[data-field="camera-tile"] tf-video-stream').shadowRoot.querySelector('video');
    const box = v.getBoundingClientRect();
    const fit = getComputedStyle(v).objectFit;
    const vr = v.videoWidth / v.videoHeight;
    const br = box.width / box.height;
    let w = box.width;
    let h = box.height;
    if (fit === 'contain') {
      if (br > vr) w = box.height * vr; else h = box.width / vr;
    } else if (fit === 'cover') {
      if (br > vr) h = box.width / vr; else w = box.height * vr;
    }
    return { fit, boxW: box.width, boxH: box.height, frameW: w, frameH: h };
  });
}

// How far the media tiles reach: their bottom edge vs the scroll container's
// visible bottom (minus its padding).
async function tileFill(page) {
  return page.evaluate(() => {
    const main = document.querySelector('.main');
    const mr = main.getBoundingClientRect();
    const pad = parseFloat(getComputedStyle(main).paddingBottom) || 0;
    const visibleBottom = mr.top + main.clientHeight - pad;
    const cam = document.querySelector('[data-field="camera-tile"]').getBoundingClientRect();
    const lid = document.querySelector('[data-field="lidar-tile"]')?.getBoundingClientRect();
    const side = document.querySelector('.robots-overview-side')?.getBoundingClientRect();
    return {
      camBottomGap: Math.round(visibleBottom - cam.bottom),
      lidBottomGap: lid ? Math.round(visibleBottom - lid.bottom) : null,
      sideBottomGap: side ? Math.round(visibleBottom - side.bottom) : null,
      camH: Math.round(cam.height),
      scrollTop: main.scrollTop,
    };
  });
}

function openSubs(page, prefix) {
  return page.evaluate((p) => window.__robotsMock.subs.filter((s) => s.open && s.id.startsWith(p)).length, prefix);
}

test.describe('Robots detail — layout, camera fit, LiDAR/camera streams', () => {
  test('tiles fill the viewport and follow a resize; camera frame is never cropped', async ({ page }) => {
    test.setTimeout(120000);
    await page.setViewportSize({ width: 1440, height: 900 });
    await loginAsAdmin(page, { port: PORT });
    await openRobotDetail(page);
    await feedSyntheticCamera(page);
    await page.waitForTimeout(500);

    for (const [w, h] of [[1440, 900], [1280, 1000], [1700, 700], [900, 1100]]) {
      await page.setViewportSize({ width: w, height: h });
      await page.waitForTimeout(400);
      const fill = await tileFill(page);
      // Wide: camera and 3D tiles end at the bottom of the visible area (a few px
      // of rounding). Narrow: the telemetry column sits under the tiles and the
      // column ends there instead.
      if (w > 1100) {
        expect(Math.abs(fill.camBottomGap), `camera tile gap @${w}x${h}`).toBeLessThanOrEqual(4);
        expect(Math.abs(fill.lidBottomGap), `3D tile gap @${w}x${h}`).toBeLessThanOrEqual(4);
      } else {
        expect(Math.abs(fill.sideBottomGap), `side column gap @${w}x${h}`).toBeLessThanOrEqual(4);
        expect(fill.camH, `camera tile height @${w}x${h}`).toBeGreaterThanOrEqual(260);
      }
      const g = await cameraFrameGeometry(page);
      expect(g.fit).toBe('contain');
      // The frame fits inside the box: nothing is cut off on either axis.
      expect(g.frameW).toBeLessThanOrEqual(g.boxW + 0.5);
      expect(g.frameH).toBeLessThanOrEqual(g.boxH + 0.5);
      // And it touches the box on one axis (letterbox, not a shrunken frame).
      expect(Math.min(Math.abs(g.frameW - g.boxW), Math.abs(g.frameH - g.boxH))).toBeLessThanOrEqual(0.5);
      await page.screenshot({ path: `${SHOT_DIR}/detail-${w}x${h}.png` });
    }

    // The full-size Kamera tab uses the same measured height.
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.locator('tf-tab#camera').click();
    await expect(page.locator('[data-field="camera-tile"].robots-tile-full')).toBeVisible();
    await page.waitForTimeout(400);
    const tabFill = await tileFill(page);
    expect(Math.abs(tabFill.camBottomGap)).toBeLessThanOrEqual(4);
    await page.screenshot({ path: `${SHOT_DIR}/camera-tab.png` });
  });

  test('no tab scrolls the page: the detail fits the window at any size', async ({ page }) => {
    test.setTimeout(180000);
    await page.setViewportSize({ width: 1440, height: 900 });
    await loginAsAdmin(page, { port: PORT });
    await openRobotDetail(page);
    const tabs = ['overview', 'camera', 'lidar', 'control', 'info', 'log'];
    for (const [w, h] of [[1440, 900], [1920, 1080], [1280, 720], [1000, 800]]) {
      await page.setViewportSize({ width: w, height: h });
      for (const tab of tabs) {
        await page.locator(`tf-tab#${tab}`).click();
        await page.waitForTimeout(350);
        const m = await page.evaluate(() => {
          const main = document.querySelector('.main');
          return { scroll: main.scrollHeight, client: main.clientHeight };
        });
        expect(m.scroll, `page scrolls on '${tab}' @${w}x${h}`).toBeLessThanOrEqual(m.client + 1);
        if (tab === 'overview' || tab === 'control') {
          await page.screenshot({ path: `${SHOT_DIR}/fit-${tab}-${w}x${h}.png` });
        }
      }
    }
  });

  test('gesture reactions toggle sends gestures_on and does not bounce on stale polls', async ({ page }) => {
    test.setTimeout(120000);
    await page.setViewportSize({ width: 1440, height: 900 });
    await loginAsAdmin(page, { port: PORT });
    await openRobotDetail(page);

    const gesturesSwitch = page.locator('[data-field="camera-tile"] [data-gestures-toggle] [role="switch"]').first();
    await expect(gesturesSwitch).toHaveAttribute('aria-checked', 'false');
    await gesturesSwitch.click();

    // The "server" still reports gestures off for two 4 s polls.
    const samples = [];
    const t0 = Date.now();
    while (Date.now() - t0 < 9000) {
      samples.push(await gesturesSwitch.getAttribute('aria-checked'));
      await page.waitForTimeout(150);
    }
    expect(samples.filter((s) => s === 'false'), `samples: ${samples.join(',')}`).toEqual([]);
    expect(await page.evaluate(() => window.__robotsMock.controls)).toEqual(['gestures_on']);
    // Gesture kinds never render as generic control buttons.
    await page.locator('tf-tab#control').click();
    await expect(page.locator('[data-control="gestures_on"], [data-control="gestures_off"], [data-control="lidar_on"]'))
      .toHaveCount(0);
    await page.locator('tf-tab#overview').click();

    // The server catches up; the toggle stays on and the pending choice clears.
    await page.evaluate(() => { window.__robotsMock.robot.gesturesEnabled = true; });
    await page.waitForTimeout(4500);
    await expect(page.locator('[data-field="camera-tile"] [data-gestures-toggle] [role="switch"]').first())
      .toHaveAttribute('aria-checked', 'true');
  });

  test('LiDAR + camera depth on by default; LiDAR off keeps the camera and does not bounce', async ({ page }) => {
    test.setTimeout(120000);
    await page.setViewportSize({ width: 1440, height: 900 });
    await loginAsAdmin(page, { port: PORT });
    await openRobotDetail(page);

    // Defaults: LiDAR frames, LiDAR scene map and the camera-depth cloud.
    await expect.poll(() => openSubs(page, 'lidar:')).toBe(1);
    await expect.poll(() => openSubs(page, 'scene:')).toBe(1);
    await expect.poll(() => openSubs(page, 'scene-depth:')).toBe(1);
    const lidarSwitch = page.locator('[data-lidar-toggle] [role="switch"]').first();
    const depthSwitch = page.locator('[data-depth-toggle] [role="switch"]').first();
    await expect(lidarSwitch).toHaveAttribute('aria-checked', 'true');
    await expect(depthSwitch).toHaveAttribute('aria-checked', 'true');

    // Turn LiDAR off while the "server" keeps reporting it on (stale list cache).
    await lidarSwitch.click();
    const samples = [];
    const t0 = Date.now();
    while (Date.now() - t0 < 9000) {
      samples.push(await lidarSwitch.getAttribute('aria-checked'));
      await page.waitForTimeout(150);
    }
    // Never bounced back on, through two 4 s polls that still said "enabled".
    expect(samples.filter((s) => s === 'true'), `samples: ${samples.join(',')}`).toEqual([]);
    expect(await page.evaluate(() => window.__robotsMock.controls)).toEqual(['lidar_off']);
    // LiDAR streams closed, camera depth still streaming and still on.
    expect(await openSubs(page, 'lidar:')).toBe(0);
    expect(await openSubs(page, 'scene:')).toBe(0);
    expect(await openSubs(page, 'scene-depth:')).toBe(1);
    await expect(depthSwitch).toHaveAttribute('aria-checked', 'true');
    await expect(depthSwitch).not.toHaveAttribute('aria-disabled', 'true');
    await page.screenshot({ path: `${SHOT_DIR}/lidar-off.png` });

    // The server catches up; the toggle stays off and the pending choice clears.
    await page.evaluate(() => { window.__robotsMock.robot.lidar.enabled = false; });
    await page.waitForTimeout(4500);
    await expect(lidarSwitch).toHaveAttribute('aria-checked', 'false');

    // Back on: LiDAR streams reopen, camera depth untouched.
    await lidarSwitch.click();
    await expect.poll(() => openSubs(page, 'lidar:')).toBe(1);
    await expect.poll(() => openSubs(page, 'scene:')).toBe(1);
    expect(await openSubs(page, 'scene-depth:')).toBe(1);
    await expect(lidarSwitch).toHaveAttribute('aria-checked', 'true');

    // Camera depth off on its own leaves LiDAR alone.
    await depthSwitch.click();
    await expect.poll(() => openSubs(page, 'scene-depth:')).toBe(0);
    expect(await openSubs(page, 'lidar:')).toBe(1);
  });
});
