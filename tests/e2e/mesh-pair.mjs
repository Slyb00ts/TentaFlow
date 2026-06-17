import { chromium } from 'playwright';

const A_URL = 'https://localhost:8095/';
const B_URL = 'https://192.168.11.26:8090/';
const CREDS = { username: 'admin', password: 'admin123' };

async function login(page, url, label) {
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  // poczekaj az codec/wasm gotowe — import shim i zaloguj przez protokol binarny
  const res = await page.evaluate(async (creds) => {
    const { ApiBinary, initTransport } = await import('/js/protocol/api-binary-shim.js');
    await initTransport();
    const r = await ApiBinary.action('authLoginRequest', creds);
    if (r && r.jwt) await ApiBinary.setJwt(r.jwt);
    const me = await ApiBinary.one('authMeRequest').catch((e) => ({ error: String(e) }));
    const id = await ApiBinary.one('meshIdentityRequest').catch((e) => ({ error: String(e) }));
    return { hasJwt: ApiBinary.hasJwt(), me, identity: id };
  }, CREDS);
  console.log(`[${label}] login:`, JSON.stringify(res.me?.username || res.me), 'node_id=', res.identity?.nodeId || res.identity?.node_id || JSON.stringify(res.identity));
  return res;
}

async function evalApi(page, fn, arg) {
  return page.evaluate(async ({ fnStr, arg }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    // eslint-disable-next-line no-new-func
    const f = new Function('ApiBinary', 'arg', `return (${fnStr})(ApiBinary, arg);`);
    return f(ApiBinary, arg);
  }, { fnStr: fn.toString(), arg });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
  const pageA = await ctx.newPage();
  const pageB = await ctx.newPage();
  pageA.on('console', (m) => { if (m.type() === 'error') console.log('[A console]', m.text()); });
  pageB.on('console', (m) => { if (m.type() === 'error') console.log('[B console]', m.text()); });

  try {
    const a = await login(pageA, A_URL, 'A');
    const b = await login(pageB, B_URL, 'B');

    const A_ID = a.identity?.nodeId || a.identity?.node_id;
    const B_ID = b.identity?.nodeId || b.identity?.node_id;
    console.log('A_ID=', A_ID);
    console.log('B_ID=', B_ID);
    if (!A_ID || !B_ID) throw new Error('missing node identity');

    // A widzi B w mesh node list?
    const nodesA = await evalApi(pageA, (Api) => Api.list('meshNodeListRequest', { arrayKey: 'nodes' }));
    console.log('[A] mesh nodes:', nodesA.map((n) => (n.nodeId || n.node_id || '').slice(0, 16)));

    // 1) A inicjuje parowanie z B (po istniejacym mesh stream)
    const startRes = await evalApi(pageA, (Api, id) => Api.action('meshPairingStartRequest', { remoteAddress: id }), B_ID);
    console.log('[A] pairing start ->', JSON.stringify(startRes));
    const pin = startRes.pin;
    const completed = startRes.completed;

    if (!completed) {
      if (!pin) throw new Error('no PIN returned and not completed');
      // 2) B: czekaj az pojawi sie pending od A, potem confirm
      let confirmed = false;
      for (let i = 0; i < 20; i++) {
        const pending = await evalApi(pageB, (Api) => Api.list('meshPendingListRequest', { arrayKey: 'pending' }));
        const ids = pending.map((p) => (p.remoteNodeId || p.remote_node_id || '').slice(0, 16));
        console.log(`[B] pending (try ${i}):`, JSON.stringify(ids));
        const match = pending.find((p) => (p.remoteNodeId || p.remote_node_id) === A_ID);
        if (match) {
          const conf = await evalApi(pageB, (Api, args) => Api.action('meshPairingConfirmRequest', { pairId: args.id, pin: args.pin }), { id: A_ID, pin });
          console.log('[B] pairing confirm ->', JSON.stringify(conf));
          confirmed = true;
          break;
        }
        await sleep(1000);
      }
      if (!confirmed) throw new Error('B never saw pending pairing from A');
    }

    // 3) poczekaj na propagacje PairingConfirm do A + bootstrap
    await sleep(3000);

    // 4) weryfikacja trust po obu stronach
    const nodesA2 = await evalApi(pageA, (Api) => Api.list('meshNodeListRequest', { arrayKey: 'nodes' }));
    const nodesB2 = await evalApi(pageB, (Api) => Api.list('meshNodeListRequest', { arrayKey: 'nodes' }));
    const fmt = (ns) => ns.map((n) => ({ id: (n.nodeId || n.node_id || '').slice(0, 16), trusted: n.trusted, status: n.status }));
    console.log('[A] nodes after:', JSON.stringify(fmt(nodesA2)));
    console.log('[B] nodes after:', JSON.stringify(fmt(nodesB2)));
    console.log('PAIRING_DONE');
  } catch (e) {
    console.error('PAIRING_ERROR:', e?.message || e);
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
