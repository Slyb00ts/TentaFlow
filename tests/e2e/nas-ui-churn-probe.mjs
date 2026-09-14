// Diagnostic probe for the TentaNas screens: how much DOM does a poll rewrite,
// which COLUMN does it, does the live chart actually advance, and is the SMART
// banner on screen? Kept in the repo because every number in the 2026-09-13
// review came from it and the next regression will need the same evidence.
//
// Every record carries a timestamp (ms since the observation started), so a
// burst can be told apart from a per-poll drip — the earlier version lacked
// this and left "29 rewrites of the Zdrowie column" ambiguous between one pass
// and six per poll.
//
// Usage (from tests/e2e, where playwright lives):
//   node nas-ui-churn-probe.mjs <base-url> <password> [out-dir]
//
// Notes learned the hard way, do not undo:
//   * tf-stream-chart renders into the LIGHT DOM (TfCartesianChart extends
//     HTMLElement; nothing in the chain calls attachShadow). Series polylines
//     live in `g.tf-chart__stream-layer`.
//   * The slide is a CSS transition: the inline transform is written ONCE to
//     its target and the browser interpolates, so `style.transform` makes a
//     smooth slide look frozen. Only getComputedStyle moves.
//   * On a node with mesh peers the landing "Przegląd" is the FLEET view and
//     carries no chart; a node card has to be clicked first.

import { chromium } from 'playwright';
import { mkdirSync, writeFileSync } from 'node:fs';

const [base, password, outDir = 'churn-probe-out'] = process.argv.slice(2);
mkdirSync(outDir, { recursive: true });

const OBSERVE_MS = 21000; // four 5 s polls plus slack

function startProbe() {
  const W = (window.__churn = { mut: [], chart: [], banner: [], t0: performance.now(), roots: 0 });
  const now = () => Math.round(performance.now() - W.t0);

  const desc = (el) => {
    if (!el || el.nodeType !== 1) return '(text)';
    const id = el.id ? '#' + el.id : '';
    const cls = typeof el.className === 'string' && el.className
      ? '.' + el.className.trim().split(/\s+/).slice(0, 2).join('.') : '';
    return el.tagName.toLowerCase() + id + cls;
  };
  const region = (n) => {
    let p = n.nodeType === 1 ? n : n.parentElement;
    for (let i = 0; p && i < 8; i += 1) {
      if (p.id) return '#' + p.id;
      if (p.tagName && p.tagName.includes('-')) return p.tagName.toLowerCase();
      p = p.parentElement || (p.getRootNode() instanceof ShadowRoot ? p.getRootNode().host : null);
    }
    return desc(n);
  };
  // For a <td>, which column is it? Header text beats an index nobody can read.
  const columnOf = (node) => {
    const td = node.nodeType === 1 && node.tagName === 'TD' ? node : null;
    if (!td || !td.parentElement) return null;
    const idx = [...td.parentElement.children].indexOf(td);
    const root = td.getRootNode();
    const ths = root && root.querySelectorAll ? root.querySelectorAll('thead th') : [];
    const th = ths[idx];
    return { idx, label: th ? th.textContent.replace(/\s+/g, ' ').trim().slice(0, 20) : '?' };
  };

  const opts = { childList: true, subtree: true, attributes: true, characterData: true };
  const obs = new MutationObserver((recs) => {
    const t = now();
    for (const r of recs) {
      const a = [...r.addedNodes].filter((x) => x.nodeType === 1).length;
      const d = [...r.removedNodes].filter((x) => x.nodeType === 1).length;
      if (r.type === 'childList' && a === 0 && d === 0) continue;
      if (W.mut.length >= 9000) continue;
      W.mut.push({ t, kind: r.type, region: region(r.target), target: desc(r.target), column: columnOf(r.target), added: a, removed: d });
    }
  });
  obs.observe(document.body, opts);
  const walk = (root) => {
    for (const el of root.querySelectorAll('*')) {
      if (el.shadowRoot) { W.roots += 1; obs.observe(el.shadowRoot, opts); walk(el.shadowRoot); }
    }
  };
  walk(document);

  const chartSig = () => {
    const hosts = [...document.querySelectorAll('tf-stream-chart')];
    if (!hosts.length) return null;
    return hosts.map((h) => {
      const layer = h.querySelector('g.tf-chart__stream-layer');
      const pts = [...h.querySelectorAll('polyline')].map((p) => (p.getAttribute('points') || '').slice(-90)).join(';');
      const tr = layer ? (getComputedStyle(layer).transform || '') : 'nolayer';
      return pts + '::' + tr;
    }).join(' || ');
  };
  W.chartShape = [...document.querySelectorAll('tf-stream-chart')].map((h) => ({
    id: h.id,
    svg: !!h.querySelector('svg'),
    layer: !!h.querySelector('g.tf-chart__stream-layer'),
    polylines: h.querySelectorAll('polyline').length,
  }));
  // Declared INSIDE the probe, and handed back by collect(): this function is
  // serialised into the page by page.evaluate, so NOTHING from the Node module
  // scope exists here. The first version read a module constant and died with
  // "SAMPLE_MS is not defined" on its very first run. Returning the value
  // instead of duplicating it keeps the freeze arithmetic from drifting.
  const sampleMs = 100;
  W.sampleMs = sampleMs;
  W.chartTimer = setInterval(() => W.chart.push({ t: now(), sig: chartSig() }), sampleMs);

  let seq = 0;
  const stamp = (el) => { if (!el.__churnId) el.__churnId = ++seq; return el.__churnId; };
  W.bannerTimer = setInterval(() => {
    const found = [];
    const scan = (root) => {
      for (const el of root.querySelectorAll('*')) {
        if (el.shadowRoot) scan(el.shadowRoot);
        if (el.children.length) continue;
        const tx = (el.textContent || '').trim();
        if (/SMART/i.test(tx)) found.push({ id: stamp(el), text: tx.slice(0, 70) });
      }
    };
    scan(document);
    W.banner.push({ t: now(), n: found.length, ids: found.map((f) => f.id).join(','), text: found[0] ? found[0].text : '' });
  }, 250);
}

function collect() {
  const W = window.__churn;
  clearInterval(W.chartTimer);
  clearInterval(W.bannerTimer);
  return { mut: W.mut, chart: W.chart, banner: W.banner, roots: W.roots, chartShape: W.chartShape, sampleMs: W.sampleMs };
}

function report(name, d) {
  if (!d) return;
  console.log(`\n===== ${name} =====`);
  const structural = d.mut.filter((m) => m.kind === 'childList');
  console.log(`shadow roots: ${d.roots} | structural: ${structural.length} (+${structural.reduce((a, m) => a + m.added, 0)} -${structural.reduce((a, m) => a + m.removed, 0)}) | attributes: ${d.mut.filter((m) => m.kind === 'attributes').length}`);

  const by = (key) => {
    const out = {};
    for (const m of structural) {
      const k = key(m);
      if (k == null) continue;
      out[k] = out[k] || { n: 0, added: 0, removed: 0, first: m.t, last: m.t };
      out[k].n += 1; out[k].added += m.added; out[k].removed += m.removed;
      out[k].first = Math.min(out[k].first, m.t); out[k].last = Math.max(out[k].last, m.t);
    }
    return out;
  };

  const regions = by((m) => m.region);
  if (Object.keys(regions).length) {
    console.log('rewritten regions:');
    for (const [k, v] of Object.entries(regions).sort((a, b) => (b[1].added + b[1].removed) - (a[1].added + a[1].removed)).slice(0, 8)) {
      console.log(`   ${k.padEnd(22)} n=${String(v.n).padStart(4)} +${v.added} -${v.removed}   ${v.first}..${v.last} ms`);
    }
  }

  const cols = by((m) => (m.column ? `${m.column.idx}:${m.column.label}` : null));
  if (Object.keys(cols).length) {
    console.log('rewritten table columns (first..last tells a burst from a per-poll drip):');
    for (const [k, v] of Object.entries(cols).sort((a, b) => b[1].n - a[1].n)) {
      console.log(`   ${k.padEnd(24)} n=${String(v.n).padStart(4)}   ${v.first}..${v.last} ms`);
    }
  }

  // One bucket per second makes the poll cadence visible at a glance.
  const buckets = {};
  for (const m of structural) buckets[Math.floor(m.t / 1000)] = (buckets[Math.floor(m.t / 1000)] || 0) + 1;
  console.log('per second:', Object.entries(buckets).map(([s, n]) => `${s}s:${n}`).join(' ') || '(none)');

  console.log('chart shape:', JSON.stringify(d.chartShape));
  const samples = d.chart.filter((c) => c.sig != null);
  if (!samples.length) console.log('chart: none on this screen');
  else {
    let advanced = 0, freeze = 0, worst = 0;
    for (let i = 1; i < samples.length; i += 1) {
      if (samples[i].sig === samples[i - 1].sig) { freeze += 1; worst = Math.max(worst, freeze); } else { advanced += 1; freeze = 0; }
    }
    console.log(`chart: ${samples.length} samples / ${samples[samples.length - 1].t - samples[0].t} ms, advanced ${advanced}x, longest freeze ${worst * (d.sampleMs || 100)} ms`);
  }

  const present = d.banner.filter((b) => b.n > 0);
  const sets = new Set(d.banner.map((b) => b.ids).filter(Boolean));
  console.log(`SMART text: ${present.length}/${d.banner.length} samples, distinct node sets ${sets.size}${present.length ? ` — "${present[0].text}"` : ''}`);
}

const browser = await chromium.launch();
const ctx = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1600, height: 1000 } });
const page = await ctx.newPage();
const settle = (ms) => page.waitForTimeout(ms);

await page.goto(base, { waitUntil: 'domcontentloaded', timeout: 60000 });
await settle(4000);
await page.locator('input[type="text"], input:not([type])').first().fill('admin', { timeout: 15000 });
const pass = page.locator('input[type="password"]').first();
await pass.fill(password, { timeout: 15000 });
await pass.press('Enter');
await settle(5000);

const entry = page.locator('#app-sidebar').getByText(/^TentaNas$/i).first();
if (await entry.count()) { try { await entry.click({ timeout: 8000 }); } catch { /* tile fallback below */ } }
await settle(5000);
const card = page.locator('.node-card[data-node]').first();
if (await card.count()) { await card.click({ timeout: 10000 }); await settle(5000); console.log('opened a node card'); }
else console.log('no node card — measuring whatever screen is up');

console.log(`observing the node overview for ${OBSERVE_MS / 1000}s ...`);
await page.evaluate(startProbe);
await settle(OBSERVE_MS);
const overview = await page.evaluate(collect);
await page.screenshot({ path: `${outDir}/overview.png`, fullPage: true });

let disks = null;
try {
  await page.getByText(/^Dyski$/i).first().click({ timeout: 10000 });
  await settle(5000);
  console.log(`observing the disks tab for ${OBSERVE_MS / 1000}s ...`);
  await page.evaluate(startProbe);
  await settle(OBSERVE_MS);
  disks = await page.evaluate(collect);
  await page.screenshot({ path: `${outDir}/disks.png`, fullPage: true });
} catch (e) {
  console.log('disks tab: ' + e.message.slice(0, 140));
}

report('NODE OVERVIEW', overview);
report('DISKS', disks);
writeFileSync(`${outDir}/churn.json`, JSON.stringify({ overview, disks }, null, 2));
console.log('\nwrote ' + outDir + '/churn.json');
await browser.close();
