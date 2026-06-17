import { chromium } from 'playwright';
import { readFileSync } from 'fs';

const A_URL = 'https://localhost:8095/';
const CREDS = { username: 'power1', password: 'power123' };
const MODEL_ID = '055b4dc8-6e16-4492-bbeb-3bd9058c2bfa'; // wytrenowany na Node B
const IMAGE_PATH = '/tmp/detect-test-1280.jpg';

const imageB64 = readFileSync(IMAGE_PATH).toString('base64');

(async () => {
  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
  const page = await ctx.newPage();
  page.on('console', (m) => { if (m.type() === 'error') console.log('[A console]', m.text()); });
  try {
    await page.goto(A_URL, { waitUntil: 'domcontentloaded' });
    const login = await page.evaluate(async (creds) => {
      const { ApiBinary, initTransport } = await import('/js/protocol/api-binary-shim.js');
      await initTransport();
      const r = await ApiBinary.action('authLoginRequest', creds);
      if (r && r.jwt) await ApiBinary.setJwt(r.jwt);
      return { user: (await ApiBinary.one('authMeRequest'))?.username };
    }, CREDS);
    console.log('[A] login:', JSON.stringify(login));

    const res = await page.evaluate(async (arg) => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      return ApiBinary.action('mlStudioRecogDetectRequest',
        { modelId: arg.modelId, threshold: 0.4, imageB64: arg.imageB64 },
        { timeoutMs: 180000 });
    }, { modelId: MODEL_ID, imageB64 });

    const dets = (() => { try { return JSON.parse(res.detectionsJson ?? res.detections_json ?? '[]'); } catch { return res.detectionsJson; } })();
    console.log('[A] detect result: width=', res.width, 'height=', res.height, 'error=', res.error);
    console.log('[A] detections count:', Array.isArray(dets) ? dets.length : 'n/a');
    if (Array.isArray(dets)) dets.slice(0, 8).forEach((d, i) => console.log(`  [${i}]`, JSON.stringify(d)));
    console.log(res.error ? 'DETECT_ERROR' : 'DETECT_DONE');
    if (res.error) process.exitCode = 1;
  } catch (e) {
    console.error('DETECT_EXC:', e?.message || e);
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
