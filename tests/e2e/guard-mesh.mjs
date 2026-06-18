import { chromium } from 'playwright';
import { readFileSync } from 'fs';

const A_URL = 'https://localhost:8095/';
const CREDS = { username: 'power1', password: 'power123' };
const B_ID = '3b226fa884a5de60a03602223397975fbc84164de79847e8c4fc3ff4f55f1404';
const PROJECT_ID = '955a7407-4911-43df-9e5d-dcbd3b9ab010';
const GUARD = readFileSync('/tmp/guard1000.jsonl');

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
    const user = await page.evaluate(async (creds) => {
      const { ApiBinary, initTransport } = await import('/js/protocol/api-binary-shim.js');
      await initTransport();
      const r = await ApiBinary.action('authLoginRequest', creds);
      if (r && r.jwt) await ApiBinary.setJwt(r.jwt);
      return (await ApiBinary.one('authMeRequest'))?.username;
    }, CREDS);
    console.log('[A] login:', user);

    // 1) Wgranie guard dataset do programu (ML Studio) przez protokół binarny.
    const up = await evalApi(page, (Api, a) => Api.action('mlStudioDatasetUploadRequest', {
      projectId: a.pid, name: 'guard-1000', filename: 'guard1000.jsonl', bytes: a.bytes,
    }), { pid: PROJECT_ID, bytes: Array.from(GUARD) });
    const datasetId = up.dataset?.datasetId ?? up.dataset?.dataset_id ?? up.datasetId ?? up.dataset_id;
    console.log('[A] guard dataset uploaded:', datasetId, 'rows=', up.dataset?.rowCount ?? up.dataset?.row_count);
    if (!datasetId) throw new Error('no datasetId from upload: ' + JSON.stringify(up).slice(0, 200));

    // 2) Trening guard SFT na 7 GPU riga przez mesh (multi-GPU + flash-attn).
    const start = await evalApi(page, (Api, a) => Api.action('mlStudioFtTrainStartRequest', {
      projectId: a.pid, datasetId: a.did,
      baseModel: 'Qwen/Qwen2.5-0.5B-Instruct',
      method: 'lora', objective: 'sft',
      targetNodeId: a.bid, numGpus: 0,
      hyperparams: { epochs: 2, batchSize: 4, gradAccumSteps: 1, learningRate: 2e-4, maxSeqLen: 512, loraR: 16, loraAlpha: 32, loraDropout: 0.05 },
    }, { timeoutMs: 180000 }), { pid: PROJECT_ID, did: datasetId, bid: B_ID });
    console.log('[A] guard train ->', JSON.stringify(start));
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
        if (status === 'succeeded') console.log('[A] GUARD_RUN_ID=' + runId);
        break;
      }
    }
    console.log('GUARD_DONE');
  } catch (e) {
    console.error('GUARD_ERR:', e?.message || e);
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
