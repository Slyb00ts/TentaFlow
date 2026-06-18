import { chromium } from 'playwright';

const A_URL = 'https://localhost:8095/';
const CREDS = { username: 'power1', password: 'power123' };
const B_ID = '3b226fa884a5de60a03602223397975fbc84164de79847e8c4fc3ff4f55f1404';
const PROJECT_ID = '955a7407-4911-43df-9e5d-dcbd3b9ab010';
const DATASET_ID = '469bf548-f3df-4e12-8e7f-c70945806215';

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
  page.on('console', (m) => { if (m.type() === 'error') console.log('[A err]', m.text()); });
  try {
    await page.goto(A_URL, { waitUntil: 'domcontentloaded' });
    const login = await page.evaluate(async (creds) => {
      const { ApiBinary, initTransport } = await import('/js/protocol/api-binary-shim.js');
      await initTransport();
      const r = await ApiBinary.action('authLoginRequest', creds);
      if (r && r.jwt) await ApiBinary.setJwt(r.jwt);
      return (await ApiBinary.one('authMeRequest'))?.username;
    }, CREDS);
    console.log('[A] login:', login);

    const start = await evalApi(page, (Api, a) => Api.action('mlStudioFtTrainStartRequest', {
      projectId: a.PROJECT_ID,
      datasetId: a.DATASET_ID,
      baseModel: 'Qwen/Qwen2.5-0.5B-Instruct',
      method: 'lora',
      objective: 'sft',
      targetNodeId: a.B_ID,
      numGpus: 0,
      dist: undefined,
      hyperparams: { epochs: 1, batchSize: 2, gradAccumSteps: 1, learningRate: 2e-4, maxSeqLen: 256, loraR: 8, loraAlpha: 16, loraDropout: 0.05 },
    }, { timeoutMs: 180000 }), { PROJECT_ID, DATASET_ID, B_ID });
    console.log('[A] ft start ->', JSON.stringify(start));
    const runId = start.runId || start.run_id;
    if (!runId) throw new Error('no runId');

    let last = '';
    for (let i = 0; i < 120; i++) {
      await sleep(8000);
      const st = await evalApi(page, (Api, id) => Api.one('mlStudioFtTrainStatusRequest', { runId: id }), runId).catch((e) => ({ error: String(e) }));
      const status = st.status || st.error;
      if (status === 'syncing') {
        const sent = Number(st.syncBytesSent ?? 0), tot = Number(st.syncBytesTotal ?? 0), rate = Number(st.syncRateBps ?? 0);
        console.log(`[A] poll ${i}: SYNC ${sent}/${tot}B ${(rate/1024).toFixed(0)}KB/s`);
      } else {
        const line = `status=${status} step=${st.step ?? ''} loss=${st.trainLoss ?? st.train_loss ?? ''}`;
        if (line !== last) console.log(`[A] poll ${i}: ${line}`);
        last = line;
      }
      if (['succeeded', 'failed', 'completed', 'error'].includes(String(status))) {
        console.log('[A] FINAL:', status, 'err=', st.error || '(none)');
        break;
      }
    }
    console.log('MULTIRIG_DONE');
  } catch (e) {
    console.error('MULTIRIG_ERR:', e?.message || e);
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
