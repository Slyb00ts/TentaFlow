import { chromium } from 'playwright';

const A_URL = 'https://localhost:8095/';
const CREDS = { username: 'power1', password: 'power123' };
const B_ID = '3b226fa884a5de60a03602223397975fbc84164de79847e8c4fc3ff4f55f1404';
const PROJECT_ID = '42d36027-1118-47b4-9552-2dd273a0b82e';
const DATASET_ID = 'b18cd9e4-0c3f-45f8-ab5c-2a631357a16e';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function evalApi(page, fn, arg) {
  return page.evaluate(async ({ fnStr, arg }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const f = new Function('ApiBinary', 'arg', `return (${fnStr})(ApiBinary, arg);`);
    return f(ApiBinary, arg);
  }, { fnStr: fn.toString(), arg });
}

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
      const me = await ApiBinary.one('authMeRequest').catch((e) => ({ error: String(e) }));
      return { user: me?.username, role: me?.role };
    }, CREDS);
    console.log('[A] login:', JSON.stringify(login));

    const start = await evalApi(page, (Api, a) => Api.action('mlStudioRecogTrainStartRequest', {
      projectId: a.PROJECT_ID,
      datasetId: a.DATASET_ID,
      variant: 'nano',
      targetNodeId: a.B_ID,
      hyperparams: { epochs: 3, batchSize: 4, gradAccum: 1, learningRate: 1e-4, resolution: 512, earlyStopping: false },
    }, { timeoutMs: 180000 }), { PROJECT_ID, DATASET_ID, B_ID });
    console.log('[A] train start ->', JSON.stringify(start));
    const runId = start.runId || start.run_id;
    if (!runId) throw new Error('no runId returned');

    let lastStatus = '';
    for (let i = 0; i < 80; i++) {
      await sleep(6000);
      const st = await evalApi(page, (Api, id) => Api.one('mlStudioRecogTrainStatusRequest', { runId: id }), runId).catch((e) => ({ error: String(e) }));
      const status = st.status || st.error || 'unknown';
      const curve = st.curveJson || st.curve_json || st.curve;
      let lastPt = '';
      try {
        const arr = typeof curve === 'string' ? JSON.parse(curve) : curve;
        if (Array.isArray(arr) && arr.length) lastPt = JSON.stringify(arr[arr.length - 1]);
      } catch {}
      if (status === 'syncing') {
        const ph = st.syncPhase ?? st.sync_phase;
        const sent = Number(st.syncBytesSent ?? st.sync_bytes_sent ?? 0);
        const tot = Number(st.syncBytesTotal ?? st.sync_bytes_total ?? 0);
        const rate = Number(st.syncRateBps ?? st.sync_rate_bps ?? 0);
        const pct = tot > 0 ? Math.round((sent / tot) * 100) : 0;
        console.log(`[A] poll ${i}: SYNC phase=${ph} ${sent}/${tot}B ${pct}% rate=${(rate / 1024).toFixed(0)}KB/s`);
      } else if (status !== lastStatus || lastPt) {
        console.log(`[A] poll ${i}: status=${status} last=${lastPt}`);
      }
      lastStatus = status;
      if (['succeeded', 'failed', 'completed', 'error'].includes(String(status))) {
        console.log('[A] FINAL status:', status, JSON.stringify(st).slice(0, 600));
        break;
      }
    }
    console.log('TRAIN_E2E_DONE');
  } catch (e) {
    console.error('TRAIN_E2E_ERROR:', e?.message || e);
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
