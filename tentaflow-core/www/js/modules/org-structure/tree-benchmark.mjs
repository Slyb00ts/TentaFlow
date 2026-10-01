// =============================================================================
// File: modules/org-structure/tree-benchmark.mjs
// Description: Measures the organization chart on synthetic companies of 500 and
//   2000 people with four levels expanded — the numbers that decide whether SVG
//   is enough (plan §3.3: first render < 300 ms, pan and zoom at 60 fps).
//   Not a test: run it by hand.
//   From tentaflow-core/www:
//     node --import ./js/_test-register.js js/modules/org-structure/tree-benchmark.mjs
//         model, layout and markup in Node + happy-dom (no paint, so no fps)
//     node js/modules/org-structure/tree-benchmark.mjs --browser
//         the real component in headless Chromium: first render, DOM size,
//         pan and zoom frame times
//   `--browser` takes Playwright from tests/e2e/node_modules and adds `--gpu`
//   to let Chromium use the GPU (without it the page is rasterised in software,
//   which is a pessimistic reading).
// =============================================================================

import { createRequire } from 'node:module';
import { createServer } from 'node:http';
import { readFileSync, existsSync } from 'node:fs';
import { dirname, extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const WWW = join(HERE, '..', '..', '..');
const SIZES = [500, 2000];

// Four levels below the root hold n people when the fanout is about n^(1/3).
const fanoutFor = (people) => Math.ceil(people ** (1 / 3)) + 1;

const stats = (samples) => {
  const sorted = [...samples].sort((a, b) => a - b);
  const pick = (q) => sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
  return { mean: samples.reduce((a, b) => a + b, 0) / samples.length, p95: pick(0.95), max: sorted[sorted.length - 1] };
};

const ms = (n) => `${n.toFixed(1)} ms`;

async function nodeRun() {
  await import('../../sdk-runtime/_dom-test-harness.js');
  const { buildTreeModel } = await import('./tree.js');
  const { syntheticView, t, labels } = await import('./tree-fixture.js');
  await import('../../components/tf-org-tree.js');
  const { layoutForest } = await import('./layout.js');

  for (const people of SIZES) {
    const view = syntheticView(people, fanoutFor(people));
    let t0 = performance.now();
    const model = buildTreeModel(view, { t });
    const buildMs = performance.now() - t0;

    document.body.innerHTML = '';
    const el = document.createElement('tf-org-tree');
    document.body.appendChild(el);
    el.labels = labels;
    el.model = model;
    for (const n of model.nodes) el._expanded.set(n.id, true);
    t0 = performance.now();
    el._relayout({ refit: true });
    const layoutMs = performance.now() - t0;
    el._view = { x: 0, y: 0, k: 0.85 };
    t0 = performance.now();
    el._render();
    const renderMs = performance.now() - t0;

    const roots = el._roots;
    t0 = performance.now();
    layoutForest(roots, el._m.layout);
    const pureLayoutMs = performance.now() - t0;
    console.log(`${people} people (${model.nodes.length} positions, fanout ${fanoutFor(people)}), happy-dom`);
    console.log(`  model build      ${ms(buildMs)}`);
    console.log(`  layout (${el._layout.items.length} boxes) ${ms(pureLayoutMs)} pure, ${ms(layoutMs)} with edges and minimap`);
    console.log(`  first slice      ${ms(renderMs)} (${el.querySelectorAll('.ot-card').length} cards, ${el.querySelectorAll('*').length} elements)`);
  }
}

const MIME = { '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css', '.html': 'text/html', '.json': 'application/json', '.wasm': 'application/wasm' };

function serve() {
  const server = createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    if (url.pathname === '/bench.html') {
      res.setHeader('content-type', 'text/html');
      res.end('<!doctype html><link rel="stylesheet" href="/css/controls.css"><body style="margin:0;background:#050818">'
        + '<div id="host" style="width:1400px;height:820px"></div></body>');
      return;
    }
    const file = normalize(join(WWW, url.pathname));
    if (!file.startsWith(WWW) || !existsSync(file)) {
      res.statusCode = 404;
      res.end();
      return;
    }
    res.setHeader('content-type', MIME[extname(file)] ?? 'application/octet-stream');
    res.end(readFileSync(file));
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

async function browserRun(useGpu) {
  const require = createRequire(join(WWW, '..', '..', 'tests', 'e2e', 'package.json'));
  const { chromium } = require('playwright');
  const server = await serve();
  const browser = await chromium.launch({ args: useGpu ? ['--enable-gpu', '--use-angle=metal'] : [] });
  const page = await browser.newPage({ viewport: { width: 1400, height: 820 } });
  page.on('pageerror', (e) => console.error('page error:', e.message));
  await page.goto(`http://127.0.0.1:${server.address().port}/bench.html`);

  for (const people of SIZES) {
    const result = await page.evaluate(async ({ people, fanout }) => {
      const { buildTreeModel } = await import('/js/modules/org-structure/tree.js');
      const { syntheticView, t, labels } = await import('/js/modules/org-structure/tree-fixture.js');
      await import('/js/components/tf-org-tree.js');
      const frame = () => new Promise((r) => requestAnimationFrame(r));
      const model = buildTreeModel(syntheticView(people, fanout), { t });
      const host = document.getElementById('host');
      host.innerHTML = '';
      const el = document.createElement('tf-org-tree');
      el.style.height = '820px';
      host.appendChild(el);
      el.labels = labels;
      await frame();

      // Everything expanded = every person of the four levels is laid out.
      const t0 = performance.now();
      el.model = model;
      for (const n of model.nodes) el._expanded.set(n.id, true);
      el._relayout({ refit: false });
      el._view = { x: 20, y: 20, k: 0.85 };
      el._applyView(true);
      el._render();
      const syncMs = performance.now() - t0;
      await frame();
      await frame();
      const firstPaintMs = performance.now() - t0;
      const boxes = el._layout.items.length;
      const world = { w: Math.round(el._layout.width), h: Math.round(el._layout.height) };
      const elements = el.querySelectorAll('*').length;

      const frames = async (steps, move) => {
        const times = [];
        let last = await frame();
        for (let i = 0; i < steps; i += 1) {
          move(i);
          const now = await frame();
          times.push(now - last);
          last = now;
        }
        return times;
      };
      const start = { ...el._view };
      const pan = await frames(180, (i) => {
        el._view = { k: start.k, x: start.x - i * 120, y: start.y - i * 3 };
        el._applyView();
      });
      el._view = { ...start };
      el._applyView(true);
      const zoom = await frames(120, (i) => {
        const k = 0.15 + 0.85 * Math.abs(Math.cos(i / 25));
        el._view = { k, x: 400 - 300 * k, y: 200 - 100 * k };
        el._applyView();
      });
      const timed = (k) => {
        el._view = { x: 0, y: 0, k };
        el._applyView(true);
        const from = performance.now();
        el._render();
        return performance.now() - from;
      };
      const detailRenderMs = timed(0.85);
      const cardsAtDetail = el.querySelectorAll('.ot-card').length;
      const lodRenderMs = timed(0.05);
      const lodElements = el.querySelectorAll('*').length;
      return {
        positions: model.nodes.length, boxes, world, elements, cardsAtDetail, lodElements, syncMs, firstPaintMs, pan, zoom,
        detailRenderMs, lodRenderMs,
      };
    }, { people, fanout: fanoutFor(people) });

    const line = (label, times) => {
      const s = stats(times);
      return `  ${label}: mean ${ms(s.mean)} (${(1000 / s.mean).toFixed(0)} fps), p95 ${ms(s.p95)}, worst ${ms(s.max)}`;
    };
    console.log(`${people} people (${result.positions} positions, ${result.boxes} boxes, world ${result.world.w}x${result.world.h}), Chromium${useGpu ? ' + GPU' : ' software'}`);
    console.log(`  layout + first slice ${ms(result.syncMs)}; to the first painted frame ${ms(result.firstPaintMs)}`);
    console.log(`  DOM at 85%: ${result.cardsAtDetail} cards, ${result.elements} elements (slice redraw ${ms(result.detailRenderMs)}); at 5% (unit tiles): ${result.lodElements} elements (redraw ${ms(result.lodRenderMs)})`);
    console.log(line('pan (crossing the world, slice redrawn as it leaves the drawn area)', result.pan));
    console.log(line('zoom ', result.zoom));
  }

  // What a person actually sees on opening a company-shaped chart (5-8 reports per manager).
  for (const [width, height] of [[1280, 800], [1440, 900]]) {
    await page.setViewportSize({ width, height });
    for (const people of SIZES) {
      const open = await page.evaluate(async ({ people, width, height }) => {
        const { buildTreeModel } = await import('/js/modules/org-structure/tree.js');
        const { realisticView, t, labels } = await import('/js/modules/org-structure/tree-fixture.js');
        await import('/js/components/tf-org-tree.js');
        const frame = () => new Promise((r) => requestAnimationFrame(r));
        const model = buildTreeModel(realisticView(people), { t });
        const host = document.getElementById('host');
        host.style.width = `${width - 300}px`;
        host.style.height = `${height - 260}px`;
        host.innerHTML = '';
        const el = document.createElement('tf-org-tree');
        host.appendChild(el);
        el.labels = labels;
        const from = performance.now();
        el.model = model;
        await frame();
        await frame();
        const openMs = performance.now() - from;
        const v = el._viewportWorld();
        const items = el._layout.items;
        const depthOf = (n) => { let d = 0; for (let c = n; c.parentId; c = el._nodeById.get(c.parentId)) d += 1; return d; };
        return {
          positions: model.nodes.length, levels: 1 + Math.max(...model.nodes.map(depthOf)), size: el._size,
          expandDepth: el._expandDepth, boxes: items.length, zoom: el._view.k, cardPx: 206 * el._view.k,
          clipped: items.filter((i) => i.x < v.x0 - 0.5 || i.y < v.y0 - 0.5 || i.x + i.w > v.x1 + 0.5 || i.y + i.h > v.y1 + 0.5).length,
          drawn: el.querySelectorAll('.ot-card').length, tiles: el.querySelectorAll('.ot-lod').length, openMs,
        };
      }, { people, width, height });
      console.log(`opening a company-shaped chart of ${people} people (${open.levels} levels) in ${Math.round(open.size.w)}x${Math.round(open.size.h)}px`);
      console.log(`  opens with ${open.expandDepth} levels expanded: ${open.boxes} boxes at zoom ${(open.zoom * 100).toFixed(0)}% (card ${open.cardPx.toFixed(0)} px wide), `
        + `${open.drawn} cards + ${open.tiles} tiles drawn, ${open.clipped} cut off, ${ms(open.openMs)} to painted`);
    }
  }
  await browser.close();
  server.close();
}

if (process.argv.includes('--browser')) await browserRun(process.argv.includes('--gpu'));
else await nodeRun();
