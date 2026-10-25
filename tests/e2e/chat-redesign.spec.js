// =============================================================================
// File: tests/e2e/chat-redesign.spec.js
// Description: The Chat screen as designed in mockups/chat-20261024: a three
//              cell top bar, a conversation list with a two-item menu, user
//              messages on the right, and an audio mode with a live transcript.
//              Runs against an isolated node whose Default Chat flow answers
//              through a slow OpenAI-compatible SSE endpoint, so streaming,
//              Stop and the Decode/TTFT metrics are measured on a real stream.
// =============================================================================

const { test, expect } = require('@playwright/test');
const fs = require('fs');
const http = require('http');
const os = require('os');
const path = require('path');
const { startBinary, stopBinary, waitForServer, binaryExists, baseUrl } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');

const PORT = 18331;
const MODEL_PORT = 18332;
const WORK_DIR = path.join(os.tmpdir(), `tentaflow-e2e-chat-redesign-${PORT}`);
const WWW_DIR = path.join(__dirname, '../../tentaflow-core/www');
const STORAGE_KEY = 'tentaflow_chat_conversations_v1';
const WORDS = 'Tensor parallel dzieli warstwy modelu między karty GPU, a pipeline parallel dzieli model na kolejne grupy warstw. Dzięki temu można uruchomić większy model niż mieści się na jednej karcie.'.split(' ');

let server = null;
let model = null;
let lastModelRequest = null;

// ~200 ms per token: slow enough to press Stop in the middle of an answer.
function startSlowModel() {
  return new Promise((resolve) => {
    const srv = http.createServer((req, res) => {
      let body = '';
      req.on('data', (d) => { body += d; });
      req.on('end', () => {
        if (req.url.endsWith('/models')) {
          res.setHeader('content-type', 'application/json');
          res.end(JSON.stringify({ object: 'list', data: [{ id: 'slow', object: 'model' }] }));
          return;
        }
        let j = {};
        try { j = JSON.parse(body); } catch { /* not a completion call */ }
        lastModelRequest = j;
        const asked = String(j.messages?.at(-1)?.content ?? '');
        if (asked.includes('AWARIA')) {
          res.writeHead(500, { 'content-type': 'application/json' });
          res.end(JSON.stringify({ error: { message: 'backend exploded' } }));
          return;
        }
        if (asked.includes('OSTRZEZENIE')) {
          res.writeHead(200, { 'content-type': 'text/event-stream' });
          res.end('data: {"id":"x","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"[Warning error] to zwykła odpowiedź"},"finish_reason":null}]}\n\ndata: {"id":"x","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
          return;
        }
        if (asked.includes('PUSTA')) {
          res.writeHead(200, { 'content-type': 'text/event-stream' });
          res.end('data: {"id":"x","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
          return;
        }
        res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
        const cutAfter = asked.includes('URWANY') ? 6 : Infinity;
        const code = asked.includes('KOD');
        let i = 0;
        const timer = setInterval(() => {
          if (i >= cutAfter) {
            // A dead socket mid-stream: the backend vanishes without a finish frame.
            clearInterval(timer);
            res.socket.destroy();
            return;
          }
          if (i < WORDS.length) {
            const delta = { content: code ? (i === 0 ? 'Oto kod:\n\n```python\n' : `print(${i})\n`) : (i ? ' ' : '') + WORDS[i] };
            res.write(`data: ${JSON.stringify({ id: 'x', object: 'chat.completion.chunk', choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`);
            i += 1;
          } else {
            const usage = { prompt_tokens: 10, completion_tokens: WORDS.length, total_tokens: 10 + WORDS.length };
            res.write(`data: ${JSON.stringify({ id: 'x', object: 'chat.completion.chunk', choices: [{ index: 0, delta: {}, finish_reason: 'stop' }], usage })}\n\ndata: [DONE]\n\n`);
            res.end();
            clearInterval(timer);
          }
        }, 200);
        res.on('close', () => clearInterval(timer));
      });
    });
    srv.listen(MODEL_PORT, '127.0.0.1', () => resolve(srv));
  });
}

test.use({
  ignoreHTTPSErrors: true,
  launchOptions: { args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream'] },
  permissions: ['microphone'],
  viewport: { width: 1440, height: 900 },
});

test.beforeAll(async () => {
  test.skip(!binaryExists(), 'tentaflow binary not built (target_shared/{release-fast,release,debug})');
  fs.rmSync(WORK_DIR, { recursive: true, force: true });
  fs.mkdirSync(WORK_DIR, { recursive: true });
  model = await startSlowModel();
  server = startBinary({
    port: PORT,
    db: path.join(WORK_DIR, 'chat.db'),
    home: path.join(WORK_DIR, 'home'),
    env: { TENTAFLOW_WWW_DIR: WWW_DIR },
  });
  await waitForServer(PORT, 60000);
});

test.afterAll(async () => {
  if (model) model.close();
  if (!server) return;
  const exited = new Promise((resolve) => server.once('exit', resolve));
  stopBinary(server);
  await Promise.race([exited, new Promise((r) => setTimeout(r, 10000))]);
  server = null;
});

async function api(page, action, payload) {
  return page.evaluate(async ([a, p]) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return p === null ? ApiBinary.one(a) : ApiBinary.action(a, p);
  }, [action, payload === undefined ? null : payload]);
}

let providerReady = false;

async function openChat(page, { seed = null, burst = false } = {}) {
  const errors = [];
  page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
  page.on('console', (m) => {
    // The self-signed certificate makes the service worker fetch fail on its own.
    if (m.type() === 'error' && !/SSL certificate/.test(m.text())) errors.push(`console: ${m.text()}`);
  });
  await page.addInitScript(() => {
    try { localStorage.setItem('tentaflow_lang', 'pl'); } catch { /* storage blocked */ }
    window.__sockets = [];
    const WS = window.WebSocket;
    window.WebSocket = function WrappedWebSocket(...a) {
      const s = new WS(...a);
      window.__sockets.push(s);
      return s;
    };
    window.WebSocket.prototype = WS.prototype;
    for (const k of ['CONNECTING', 'OPEN', 'CLOSING', 'CLOSED']) window.WebSocket[k] = WS[k];
  });
  if (seed) {
    await page.addInitScript(([key, value]) => {
      try { if (!localStorage.getItem(key)) localStorage.setItem(key, value); } catch { /* storage blocked */ }
    }, [STORAGE_KEY, JSON.stringify(seed)]);
  }
  if (burst) await page.addInitScript(() => { window.__burst = true; });
  await page.addInitScript(() => {
    // The OS microphone prompt never resolves under automation on macOS; a silent
    // synthetic stream keeps the real worklet and analyser path under test.
    navigator.mediaDevices.getUserMedia = async () => {
      const c = new AudioContext();
      const d = c.createMediaStreamDestination();
      const g = c.createGain();
      g.gain.value = 0;
      // One loud burst after the mic opens is what the voice-activity detector reads as an utterance.
      if (window.__burst) {
        setTimeout(() => { g.gain.value = 0.5; }, 1500);
        setTimeout(() => { g.gain.value = 0; }, 3200);
      }
      const o = c.createOscillator();
      o.connect(g).connect(d);
      o.start();
      return d.stream;
    };
  });
  await page.addInitScript(() => {
    document.addEventListener('DOMContentLoaded', () => {
      const st = document.createElement('style');
      st.textContent = '.update-overlay{display:none!important}';
      document.head.appendChild(st);
    });
  });
  await page.goto(`${baseUrl(PORT)}/`);
  await loginAsAdmin(page, { port: PORT });
  if (!providerReady) {
    const nodes = await api(page, 'meshNodeListRequest');
    const res = await api(page, 'serviceManifestDeployRequest', {
      engineId: 'openai-compatible',
      deployMethod: 'external',
      nodeId: (nodes?.nodes ?? [])[0]?.nodeId ?? '',
      configJson: JSON.stringify({
        base_url: `http://127.0.0.1:${MODEL_PORT}/v1`, api_key: 'k', auth_mode: 'api', model_repo: 'slow',
      }),
    });
    expect(res?.deployId, 'provider deploy returned no id').toBeTruthy();
    await expect.poll(async () => {
      const body = await api(page, 'serviceListRequest');
      return (body?.services ?? []).find((s) => s.engineId === 'openai-compatible')?.status ?? 'missing';
    }, { timeout: 60_000 }).toMatch(/running|degraded/);
    providerReady = true;
  }
  await page.locator('[data-view="chat"]').first().evaluate((el) => el.click());
  await page.waitForSelector('.chat-root');
  // The screen slides in: measure only once its box stopped moving.
  let prev = -1;
  await expect.poll(async () => {
    const w = await page.locator('.chat-cq').evaluate((el) => el.getBoundingClientRect().width);
    const settled = w === prev;
    prev = w;
    return settled;
  }, { intervals: [250] }).toBe(true);
  return errors;
}

function seedConversations() {
  const now = Date.now();
  const mk = (id, title, ageMs, text) => ({
    id, title, createdAt: now - ageMs, updatedAt: now - ageMs, mode: 'text',
    messages: [
      { id: `${id}-u`, role: 'user', text, ts: now - ageMs },
      { id: `${id}-a`, role: 'assistant', text: 'Odpowiedź przykładowa.', ts: now - ageMs + 1000 },
    ],
  });
  return [
    mk('c-today', 'Przydział VRAM w vLLM', 60_000, 'Jak przydzielić VRAM?'),
    mk('c-yest', 'Optymalizacja zapytania SQL', 26 * 3600_000, 'Jak przyspieszyć zapytanie?'),
    mk('c-week', 'Plan migracji bazy', 3 * 24 * 3600_000, 'Plan migracji?'),
  ];
}

async function sendAndWaitDone(page, text) {
  await page.locator('#chat-input textarea').fill(text);
  await page.locator('#chat-send').click();
  await expect(page.locator('#chat-send')).not.toHaveAttribute('data-generating', 'true', { timeout: 60_000 });
}

test.describe('Chat redesign', () => {
  test('top bar: three equal cells, nothing else; two on mobile', async ({ page }) => {
    const errors = await openChat(page);
    await expect(page.locator('.chat-top > *')).toHaveCount(3);
    const bar = await page.locator('.chat-top').boundingBox();
    expect(bar.height).toBeGreaterThanOrEqual(60);
    expect(bar.height).toBeLessThanOrEqual(62.5);
    const decode = await page.locator('#chat-decode').boundingBox();
    expect(decode.y + decode.height).toBeLessThanOrEqual(bar.y + bar.height + 0.1);
    const composer = await page.locator('#chat-composer').boundingBox();
    expect(composer.width).toBe(780);
    const boxes = await page.evaluate(() => ['#chat-decode', '#chat-ttft', '.top-actions'].map((s) => document.querySelector(s).getBoundingClientRect().toJSON()));
    const total = boxes.reduce((s, b) => s + b.width, 0);
    for (const b of boxes) expect(Math.abs(b.width / total - 1 / 3)).toBeLessThan(0.02);
    await expect(page.locator('#chat-decode')).toContainText('Decode');
    await expect(page.locator('#chat-ttft')).toContainText('TTFT');
    await expect(page.locator('#chat-mode')).toBeVisible();
    expect(errors).toEqual([]);
  });

  test('conversation list: always-visible kebab, two-item menu, rename, delete with confirmation, search, grouping', async ({ page }) => {
    const errors = await openChat(page, { seed: seedConversations() });
    const groups = await page.locator('.conv-grp').allTextContents();
    expect(groups.join('|').toLowerCase()).toMatch(/dzisiaj.*wczoraj.*ostatnie 7 dni/);

    const row = page.locator('.conv-item[data-conv-id="c-yest"]');
    const kebab = row.locator('[data-conv-more]');
    await expect(kebab).toBeVisible();
    const rb = await row.boundingBox();
    const kb = await kebab.boundingBox();
    expect(Math.abs((kb.y + kb.height / 2) - (rb.y + rb.height / 2))).toBeLessThan(3);

    await kebab.click();
    await expect(page.locator('#chat-conv-menu tf-menu-item')).toHaveCount(2);
    await expect(page.locator('#chat-conv-menu tf-menu-item[danger]')).toHaveCount(1);
    await page.keyboard.press('Escape');

    // From the keyboard: Enter on the kebab moves focus into the menu and the arrows work.
    await kebab.focus();
    await page.keyboard.press('Enter');
    await expect.poll(() => page.evaluate(() => document.querySelector('#chat-conv-menu').contains(document.activeElement))).toBe(true);
    await page.keyboard.press('ArrowDown');
    await expect.poll(() => page.evaluate(() => document.activeElement?.closest('tf-menu-item')?.getAttribute('action'))).toBe('delete');
    await page.keyboard.press('Escape');
    await kebab.click();

    // Rename.
    await page.locator('#chat-conv-menu tf-menu-item[action="rename"]').click();
    const input = page.locator('#chat-rename-input input');
    await expect(input).toBeVisible();
    await input.fill('Nowa nazwa SQL');
    await page.locator('#chat-rename-save').click();
    await expect(page.locator('.conv-item[data-conv-id="c-yest"]')).toContainText('Nowa nazwa SQL');

    // Search narrows the list.
    await page.locator('#chat-search input').fill('migracji');
    await expect(page.locator('.conv-item')).toHaveCount(1);
    await page.locator('#chat-search input').fill('');
    await expect(page.locator('.conv-item')).toHaveCount(3);

    // Delete: cancel keeps it, confirm removes it.
    await page.locator('.conv-item[data-conv-id="c-week"] [data-conv-more]').click();
    await page.locator('#chat-conv-menu tf-menu-item[action="delete"]').click();
    const dialog = page.locator('tf-window[open], tf-window').last();
    await expect(dialog).toContainText('Plan migracji bazy');
    await dialog.locator('[data-action="cancel"], [data-action="close"]').first().click();
    await expect(page.locator('.conv-item')).toHaveCount(3);
    await page.locator('.conv-item[data-conv-id="c-week"] [data-conv-more]').click();
    await page.locator('#chat-conv-menu tf-menu-item[action="delete"]').click();
    await page.locator('tf-window [data-action="confirm"], tf-window [data-action="ok"], tf-window [data-action="save"]').last().click();
    await expect(page.locator('.conv-item')).toHaveCount(2);

    // New conversation.
    await page.locator('#chat-new').click();
    await expect(page.locator('.conv-item')).toHaveCount(3);
    expect(errors).toEqual([]);
  });

  test('send streams, Stop cuts the stream, copy, regenerate, metrics, no model or flow names', async ({ page, context }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    const errors = await openChat(page);
    await page.locator('#chat-new').click();

    // Empty state offers suggestions and no model/flow selector.
    await expect(page.locator('#chat-flow, #chat-model')).toHaveCount(0);

    await page.locator('#chat-input textarea').fill('Czym różni się tensor parallel?');
    await page.locator('#chat-send').click();
    await expect(page.locator('#chat-send')).toHaveAttribute('data-generating', 'true');
    await expect(page.locator('.chat-msg.ai').last()).toContainText('Tensor', { timeout: 20_000 });

    // Stop in the middle: the stream is cut, the partial text stays, Send returns.
    await page.locator('#chat-send').click();
    await expect(page.locator('#chat-send')).not.toHaveAttribute('data-generating', 'true');
    const partial = await page.locator('.chat-msg.ai').last().innerText();
    expect(partial).toContain('Tensor');
    expect(partial).not.toContain('na jednej karcie');
    await page.waitForTimeout(1500);
    expect(await page.locator('.chat-msg.ai').last().innerText()).toBe(partial);

    // A full answer: metrics come from the real stream.
    await sendAndWaitDone(page, 'Jeszcze raz, krótko.');
    await expect(page.locator('.chat-msg.ai').last()).toContainText('na jednej karcie');
    await expect(page.locator('#m-cur')).not.toHaveText('—');
    await expect(page.locator('#m-ttft')).not.toHaveText('—');
    await page.locator('#chat-decode').hover();
    await expect(page.locator('#m-tip')).toContainText(/rozmow/i);

    // User on the right, assistant on the left.
    const u = await page.locator('.chat-msg.user .chat-msg__av').last().boundingBox();
    const a = await page.locator('.chat-msg.ai .chat-msg__av').last().boundingBox();
    const ub = await page.locator('.chat-msg.user .chat-msg__body').last().boundingBox();
    const col = await page.locator('#chat-body').boundingBox();
    expect(u.x + u.width).toBeGreaterThan(col.x + col.width - 120);
    expect(a.x).toBeLessThan(col.x + 120);
    expect(u.x).toBeGreaterThan(a.x + 200);
    expect(ub.width + u.width).toBeLessThanOrEqual(col.width * 0.82);

    // Copy puts the answer on the clipboard.
    const answer = (await page.locator('.chat-msg.ai').last().locator('.chat-msg__txt').innerText()).trim();
    await page.locator('.chat-msg.ai').last().locator('[data-act="copy"]').click();
    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toContain('Tensor parallel');
    expect(answer).toContain('Tensor parallel');

    // Regenerate asks the model again and replaces the last answer.
    lastModelRequest = null;
    await page.locator('.chat-msg.ai').last().locator('[data-act="regenerate"]').click();
    await expect(page.locator('#chat-send')).toHaveAttribute('data-generating', 'true');
    await expect(page.locator('#chat-send')).not.toHaveAttribute('data-generating', 'true', { timeout: 60_000 });
    expect(lastModelRequest).not.toBeNull();
    await expect(page.locator('.chat-msg.ai')).toHaveCount(2);

    // No model or flow name anywhere in the chat DOM.
    const text = await page.locator('.chat-root').innerText();
    expect(text).not.toMatch(/slow|openai|Default Chat|gpt|qwen|llama/i);
    expect(errors).toEqual([]);
  });

  test('Chat/Audio switch keeps the conversation; audio controls work; Esc leaves audio', async ({ page }) => {
    const errors = await openChat(page, { seed: seedConversations() });
    await page.locator('.conv-item[data-conv-id="c-today"]').click();
    await expect(page.locator('.chat-msg.user')).toHaveCount(1);

    await page.locator('#chat-mode').evaluate((el) => { el.value = 'audio'; el.dispatchEvent(new CustomEvent('change', { detail: { value: 'audio' }, bubbles: true })); });
    await expect(page.locator('#chat-face-stage')).toBeVisible();
    await expect(page.locator('#chat-face-stage tf-face')).toHaveCount(1);
    await expect(page.locator('#audio-transcript .tr-item')).toHaveCount(2);
    await expect(page.locator('#audio-transcript')).toContainText('Jak przydzielić VRAM?');
    // The whole chat/audio DOM names neither a model nor a flow.
    expect(await page.locator('.chat-root').innerText()).not.toMatch(/slow|openai|Default Chat|gpt|qwen|llama/i);
    // Before the first click only mic and exit are live.
    await expect(page.locator('#audio-mic')).not.toHaveAttribute('disabled', '');
    await expect(page.locator('#audio-pause')).toHaveAttribute('disabled', '');

    await page.locator('#audio-mic').click();
    await expect(page.locator('#audio-pause')).not.toHaveAttribute('disabled', '', { timeout: 15_000 });
    await page.locator('#audio-mic').click();
    await expect(page.locator('#audio-mic')).toHaveClass(/is-muted/);
    await page.locator('#audio-mic').click();
    await expect(page.locator('#audio-mic')).not.toHaveClass(/is-muted/);
    await page.locator('#audio-volume').click();
    await expect(page.locator('#audio-volume')).toHaveClass(/is-muted/);
    // Pause cuts the answer in progress; with nothing playing the stage keeps listening.
    await page.locator('#audio-pause').click();
    await expect(page.locator('#audio-status-label')).toHaveText('Słucham…');
    await expect(page.locator('#audio-chip')).toHaveAttribute('status', 'ok');

    await page.locator('#audio-exit').click();
    await expect(page.locator('#chat-face-stage')).toHaveCount(0);
    await expect(page.locator('.chat-msg.user')).toHaveCount(1);
    await expect(page.locator('.conv-item[data-conv-id="c-today"]')).toHaveClass(/active/);

    // Esc also leaves audio.
    await page.locator('#chat-mode').evaluate((el) => { el.value = 'audio'; el.dispatchEvent(new CustomEvent('change', { detail: { value: 'audio' }, bubbles: true })); });
    await expect(page.locator('#chat-face-stage')).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.locator('#chat-face-stage')).toHaveCount(0);
    expect(errors).toEqual([]);
  });

  test.describe('phone', () => {
    test.use({ viewport: { width: 390, height: 844 } });

    test('top bar: two cells of one half, no TTFT', async ({ page }) => {
      const errors = await openChat(page);
      await expect(page.locator('#chat-ttft')).toBeHidden();
      expect(await page.locator('#chat-mode .tf-segmented').evaluate((e) => e.getBoundingClientRect().height)).toBeCloseTo(36.5, 0);
      expect(await page.locator('.chat-top').evaluate((e) => e.getBoundingClientRect().height)).toBe(56);
      const widths = await page.evaluate(() => ['#chat-decode', '.top-actions'].map((s) => document.querySelector(s).getBoundingClientRect().width));
      expect(Math.abs(widths[0] - widths[1])).toBeLessThan(2);
      expect(widths[0]).toBeGreaterThan(150);
      expect(errors).toEqual([]);
    });

    test('Esc closes the conversation drawer', async ({ page }) => {
      const errors = await openChat(page, { seed: seedConversations() });
      await page.locator('#chat-conv-open').click();
      await expect(page.locator('#chat-shell')).toHaveClass(/conv-open/);
      await page.keyboard.press('Escape');
      await expect(page.locator('#chat-shell')).not.toHaveClass(/conv-open/);
      expect(errors).toEqual([]);
    });

    test('audio: face, pill, wave and dock only', async ({ page }) => {
      const errors = await openChat(page, { seed: seedConversations() });
      await page.locator('#chat-mode').evaluate((el) => { el.value = 'audio'; el.dispatchEvent(new CustomEvent('change', { detail: { value: 'audio' }, bubbles: true })); });
      await expect(page.locator('#chat-face-stage')).toBeVisible();
      await expect(page.locator('.transcript')).toBeHidden();
      await expect(page.locator('#audio-status')).toBeVisible();
      await expect(page.locator('.wave')).toBeVisible();
      await expect(page.locator('.audio-controls')).toBeVisible();
      const stage = await page.locator('#chat-face-stage').boundingBox();
      expect(stage.width).toBeGreaterThan(380);
      expect(errors).toEqual([]);
    });
  });

  test('connection lost mid-answer: interrupted state, resume offered, nothing measured from the cut stream', async ({ page, context }) => {
    const errors = await openChat(page);
    await page.locator('#chat-new').click();
    await page.locator('#chat-input textarea').fill('Odpowiedz powoli.');
    await page.locator('#chat-send').click();
    await expect(page.locator('.chat-msg.ai').last()).toContainText('Tensor', { timeout: 20_000 });

    await context.setOffline(true);
    await page.evaluate(() => window.__sockets.forEach((s) => { try { s.close(); } catch { /* already closed */ } }));
    const answer = page.locator('.chat-msg.ai').last();
    await expect(answer.locator('.chat-msg__error')).toContainText('Odpowiedź przerwana', { timeout: 20_000 });
    await expect(answer.locator('[data-act="resume"]')).toBeVisible();
    // A cut stream is not a finished answer: no metrics chips, no empty-answer text, partial text kept.
    await expect(answer.locator('.chat-msg__meta tf-chip')).toHaveCount(0);
    await expect(answer).toContainText('Tensor');
    await expect(answer).not.toContainText('pusta odpowiedź');
    const stored = await page.evaluate((key) => JSON.parse(localStorage.getItem(key)).flatMap((c) => c.messages).filter((m) => m.role === 'assistant'), STORAGE_KEY);
    expect(stored.at(-1).perf).toBeUndefined();
    expect(stored.at(-1).failed.kind).toBe('interrupted');

    // The platform overlay owns the reconnect UI; the chat has no banner of its own.
    await expect(page.locator('#chat-banner')).toHaveCount(0);
    await context.setOffline(false);
    await expect.poll(() => page.evaluate(() => window.__sockets.some((sock) => sock.readyState === 1)), { timeout: 60_000 }).toBe(true);
    await expect(page.locator('.conn-overlay.visible')).toHaveCount(0, { timeout: 30_000 });
    await answer.locator('[data-act="resume"]').click();
    await expect(page.locator('#chat-send')).toHaveAttribute('data-generating', 'true');
    await expect(page.locator('#chat-send')).not.toHaveAttribute('data-generating', 'true', { timeout: 60_000 });
    await expect(page.locator('.chat-msg.ai').last()).toContainText('na jednej karcie');
    await expect(page.locator('.chat-msg.ai').last().locator('.chat-msg__error')).toHaveCount(0);
    expect(errors.filter((e) => !/WebSocket|ERR_INTERNET|Failed to fetch|net::/i.test(e))).toEqual([]);
  });

  test('reload mid-answer: the orphaned bubble becomes an interrupted answer with resume', async ({ page }) => {
    await openChat(page);
    await page.locator('#chat-new').click();
    await page.locator('#chat-input textarea').fill('Odpowiedz powoli, potem odśwież.');
    await page.locator('#chat-send').click();
    await expect(page.locator('.chat-msg.ai').last()).toContainText('Tensor', { timeout: 20_000 });

    const before = (await page.locator('.chat-msg.ai').last().locator('.chat-msg__txt').innerText()).trim();
    expect(before.length).toBeGreaterThan(5);
    await page.reload();
    await page.waitForSelector('.chat-root', { timeout: 30_000 }).catch(async () => {
      await page.locator('[data-view="chat"]').first().evaluate((el) => el.click());
      await page.waitForSelector('.chat-root');
    });
    const answer = page.locator('.chat-msg.ai').last();
    await expect(answer.locator('.chat-msg__error')).toContainText('Odpowiedź przerwana');
    await expect(answer.locator('.chat-typing')).toHaveCount(0);
    // The error says the fragment is kept, so the fragment must really be there.
    const kept = (await answer.locator('.chat-msg__txt').innerText()).replace(/\s+/g, ' ');
    expect(kept).toContain('Tensor');
    expect(await page.evaluate((key) => JSON.parse(localStorage.getItem(key)).flatMap((c) => c.messages).filter((m) => m.role === 'assistant').at(-1).text.length, STORAGE_KEY)).toBeGreaterThan(5);
    await expect(page.locator('#chat-send')).not.toHaveAttribute('data-generating', 'true');
    await expect(page.locator('#chat-send')).not.toHaveAttribute('disabled', '');
    await answer.locator('[data-act="resume"]').click();
    await expect(page.locator('#chat-send')).not.toHaveAttribute('data-generating', 'true', { timeout: 60_000 });
    await expect(page.locator('.chat-msg.ai').last()).toContainText('na jednej karcie');
  });

  test('speech recognition failure: neutral message, no stuck "recognizing", back to listening', async ({ page }) => {
    // The test node has no speech model, so the flow rejects the utterance.
    const errors = await openChat(page, { burst: true });
    await page.locator('#chat-new').click();
    await page.locator('#chat-mode').evaluate((el) => { el.value = 'audio'; el.dispatchEvent(new CustomEvent('change', { detail: { value: 'audio' }, bubbles: true })); });
    await page.locator('#audio-mic').click();
    const toast = page.locator('.toast-container .toast').filter({ hasText: 'Nie udało się rozpoznać' });
    await expect(toast).toBeVisible({ timeout: 30_000 });
    expect(await toast.innerText()).not.toMatch(/stt|tts|llm|flow|model|whisper|default chat/i);
    await expect(page.locator('#audio-transcript')).not.toContainText('Rozpoznaję');
    await expect(page.locator('#audio-transcript .tr-item')).toHaveCount(0);
    await expect(page.locator('#audio-status-label')).toHaveText('Słucham…');
    expect(errors).toEqual([]);
  });

  test('a backend failure or an empty answer on a live link is an error with resume, never "(empty answer)"', async ({ page }) => {
    const errors = await openChat(page);
    await page.locator('#chat-new').click();
    for (const prompt of ['PUSTA odpowiedź proszę', 'AWARIA backendu']) {
      await page.locator('#chat-input textarea').fill(prompt);
      await page.locator('#chat-send').click();
      const answer = page.locator('.chat-msg.ai').last();
      await expect(answer.locator('.chat-msg__error')).toBeVisible({ timeout: 60_000 });
      await expect(answer.locator('[data-act="resume"]')).toBeVisible();
      await expect(answer).not.toContainText(/pusta odpowiedź|empty response/i);
      // No fragment exists, so nothing may claim one is kept.
      await expect(answer.locator('.chat-msg__error')).not.toContainText('fragment');
      await expect(answer.locator('.chat-msg__meta tf-chip')).toHaveCount(0);
      await expect(page.locator('#chat-send')).not.toHaveAttribute('data-generating', 'true');
      expect(await answer.innerText()).not.toMatch(/exploded|openai|slow|flow|model/i);
    }
    expect(errors).toEqual([]);
  });

  test('the streaming caret sits inline after the text, not on its own line', async ({ page }) => {
    await openChat(page);
    await page.locator('#chat-new').click();
    await page.locator('#chat-input textarea').fill('Odpowiedz powoli.');
    await page.locator('#chat-send').click();
    await expect(page.locator('.chat-msg.ai .chat-caret')).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('.chat-msg.ai').last()).toContainText('Tensor', { timeout: 20_000 });
    const geo = await page.evaluate(() => {
      const caret = document.querySelector('.chat-msg.ai .chat-caret');
      const txt = caret.closest('.chat-msg__txt');
      const lastBlock = txt.querySelector('p:last-of-type') || txt;
      const range = document.createRange();
      range.selectNodeContents(lastBlock);
      const rects = [...range.getClientRects()];
      const lastLine = rects.at(-1);
      const c = caret.getBoundingClientRect();
      return { caretTop: c.top, caretBottom: c.bottom, lineTop: lastLine.top, lineBottom: lastLine.bottom, insideBlock: lastBlock.contains(caret) };
    });
    expect(geo.insideBlock).toBe(true);
    expect(geo.caretTop).toBeLessThan(geo.lineBottom);
    expect(geo.caretBottom).toBeGreaterThan(geo.lineTop);
  });

  test('a truncated answer does not add to the session decode series', async ({ page, context }) => {
    await openChat(page);
    await page.locator('#chat-new').click();
    await page.locator('#chat-input textarea').fill('Odpowiedz powoli.');
    await page.locator('#chat-send').click();
    await expect(page.locator('.chat-msg.ai').last()).toContainText('Tensor', { timeout: 20_000 });
    await page.waitForTimeout(2200);
    await context.setOffline(true);
    await page.evaluate(() => window.__sockets.forEach((s) => { try { s.close(); } catch { /* already closed */ } }));
    await expect(page.locator('.chat-msg.ai').last().locator('.chat-msg__error')).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('#m-cur')).toHaveText('—');
    await context.setOffline(false);
  });

  test('streaming never restarts the entry animation: the list does not flicker', async ({ page }) => {
    await openChat(page);
    await page.locator('#chat-new').click();
    await page.locator('#chat-input textarea').fill('Odpowiedz powoli.');
    await page.locator('#chat-send').click();
    await expect(page.locator('.chat-msg.ai .chat-caret')).toBeVisible({ timeout: 20_000 });
    // Past the 0.4 s entry of the bubble itself: from here every token only patches the tail.
    await page.waitForTimeout(700);
    const trace = await page.evaluate(() => new Promise((resolve) => {
      const seen = [];
      const t0 = performance.now();
      const tick = () => {
        const els = [...document.querySelectorAll('.chat-msg')];
        seen.push({
          opacity: Math.min(...els.map((e) => Number(getComputedStyle(e).opacity))),
          animated: els.some((e) => e.getAnimations().length > 0),
        });
        if (performance.now() - t0 < 1800) requestAnimationFrame(tick); else resolve(seen);
      };
      tick();
    }));
    expect(trace.length).toBeGreaterThan(20);
    expect(trace.every((f) => f.opacity === 1)).toBe(true);
    expect(trace.some((f) => f.animated)).toBe(false);
  });

  test('a token only patches the tail: an earlier answer keeps its nodes and its text selection', async ({ page }) => {
    await openChat(page);
    await page.locator('#chat-new').click();
    await sendAndWaitDone(page, 'Pierwsze pytanie.');
    await page.locator('#chat-input textarea').fill('Drugie pytanie.');
    await page.locator('#chat-send').click();
    await expect(page.locator('.chat-msg.ai').last()).toContainText('Tensor', { timeout: 20_000 });
    // The scroll-to-bottom that follows a send settles within a moment and redraws the window while it does.
    await page.waitForTimeout(1200);
    await page.evaluate(() => {
      const first = document.querySelector('.chat-msg.ai .chat-msg__txt');
      first.__kept = true;
      const range = document.createRange();
      range.selectNodeContents(first);
      const sel = window.getSelection();
      sel.removeAllRanges();
      sel.addRange(range);
    });
    await page.waitForTimeout(1500);
    const state = await page.evaluate(() => ({
      kept: document.querySelector('.chat-msg.ai .chat-msg__txt').__kept === true,
      selected: window.getSelection().toString().length,
    }));
    expect(state.kept).toBe(true);
    expect(state.selected).toBeGreaterThan(5);
  });

  test('the Send glyph, the Decode sparkline and the row actions match the mockup sizes', async ({ page }) => {
    await openChat(page);
    await page.locator('#chat-new').click();
    await sendAndWaitDone(page, 'Odpowiedz powoli.');
    const size = (sel) => page.locator(sel).first().evaluate((e) => {
      const r = e.getBoundingClientRect();
      return [Math.round(r.width), Math.round(r.height), getComputedStyle(e).borderRadius];
    });
    expect(await size('#chat-send .tf-btn')).toEqual([40, 32, '6px']);
    expect((await size('#m-spark')).slice(0, 2)).toEqual([64, 22]);
    expect((await size('.conv-more .tf-btn')).slice(0, 2)).toEqual([28, 28]);
    expect((await size('.chat-msg__acts .tf-btn')).slice(0, 2)).toEqual([28, 28]);
    expect((await size('#chat-mode .tf-segmented'))[1]).toBe(38);
    expect((await size('.chat-top'))[1]).toBe(62);
    expect((await size('#chat-decode'))[1]).toBeLessThanOrEqual(62);
  });

  test('a streamed code fence is a code block from its first line', async ({ page }) => {
    await openChat(page);
    await page.locator('#chat-new').click();
    await page.locator('#chat-input textarea').fill('KOD proszę');
    await page.locator('#chat-send').click();
    const answer = page.locator('.chat-msg.ai').last();
    await expect(answer.locator('.code-block')).toBeVisible({ timeout: 20_000 });
    await expect(answer.locator('.chat-caret')).toBeVisible();
    expect(await answer.innerText()).not.toContain('```');
    await expect(answer.locator('.code-block tf-button.copy-btn')).toHaveCount(1);
    await expect(answer.locator('.code-block button.copy-btn')).toHaveCount(0);
    await expect(page.locator('#chat-send')).not.toHaveAttribute('data-generating', 'true', { timeout: 60_000 });
    await answer.locator('.copy-btn').click();
    await expect(page.locator('.toast-container .toast')).toContainText(/Skopiowano|skopiować/);
  });

  test('a broken backend stream never shows its raw error: neutral message, fragment kept, resume', async ({ page }) => {
    await openChat(page);
    await page.locator('#chat-new').click();
    await page.locator('#chat-input textarea').fill('URWANY strumień');
    await page.locator('#chat-send').click();
    const answer = page.locator('.chat-msg.ai').last();
    await expect(answer.locator('.chat-msg__error')).toBeVisible({ timeout: 60_000 });
    await expect(answer.locator('[data-act="resume"]')).toBeVisible();
    const text = await answer.innerText();
    expect(text).not.toMatch(/stream error|dispatcher|flow|wewn[eę]trzny|llm/i);
    await expect(answer.locator('.chat-msg__txt')).toContainText('Tensor');
    await expect(page.locator('#chat-send')).not.toHaveAttribute('data-generating', 'true');
  });

  test('any "[<words> error]" delta is a failed turn: a routing error never shows as an answer', async ({ page }) => {
    await openChat(page);
    await page.locator('#chat-new').click();
    // A flow id nobody owns makes the server answer with a "[routing error] ..." delta.
    await page.evaluate(async () => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      const subscribe = ApiBinary.subscribe.bind(ApiBinary);
      ApiBinary.subscribe = (kind, payload, handlers) => subscribe(kind, kind === 'chatStreamRequest'
        ? { ...payload, flowId: '00000000-0000-4000-8000-0000000000ff' } : payload, handlers);
    });
    await page.locator('#chat-input textarea').fill('Dokąd to trafi?');
    await page.locator('#chat-send').click();
    const answer = page.locator('.chat-msg.ai').last();
    await expect(answer.locator('.chat-msg__error')).toBeVisible({ timeout: 30_000 });
    await expect(answer.locator('[data-act="resume"]')).toBeVisible();
    expect(await answer.innerText()).not.toMatch(/routing error|stream error|flow|wewn[eę]trzny|dispatcher/i);
    await expect(page.locator('#chat-send')).not.toHaveAttribute('data-generating', 'true');
  });

  test('an answer that merely starts with a bracketed "error" word is still an answer', async ({ page }) => {
    await openChat(page);
    await page.locator('#chat-new').click();
    await sendAndWaitDone(page, 'OSTRZEZENIE proszę');
    const answer = page.locator('.chat-msg.ai').last();
    await expect(answer.locator('.chat-msg__txt')).toContainText('[Warning error] to zwykła odpowiedź');
    await expect(answer.locator('.chat-msg__error')).toHaveCount(0);
  });

  test('an audio start failure shows one translated toast, never the browser message', async ({ page }) => {
    await openChat(page);
    await page.addInitScript(() => {
      navigator.mediaDevices.getUserMedia = async () => { throw new Error('AudioWorklet: Unable to load module'); };
    });
    await page.reload();
    await page.waitForSelector('.chat-root', { timeout: 30_000 }).catch(async () => {
      await page.locator('[data-view="chat"]').first().evaluate((el) => el.click());
      await page.waitForSelector('.chat-root');
    });
    await page.locator('#chat-new').click();
    await page.locator('#chat-mode').evaluate((el) => { el.value = 'audio'; el.dispatchEvent(new CustomEvent('change', { detail: { value: 'audio' }, bubbles: true })); });
    await page.locator('#audio-mic').click();
    const toasts = page.locator('.toast-container .toast');
    await expect(toasts.first()).toHaveText('Błąd audio');
    await page.waitForTimeout(500);
    await expect(toasts).toHaveCount(1);
    expect(await toasts.first().innerText()).not.toMatch(/AudioWorklet|module/i);
  });

  test('older answers keep their Copy button in place and their selection when a stream ends', async ({ page }) => {
    await openChat(page);
    await page.locator('#chat-new').click();
    await sendAndWaitDone(page, 'Pierwsze pytanie.');
    await page.locator('#chat-input textarea').fill('Drugie pytanie.');
    await page.locator('#chat-send').click();
    await expect(page.locator('.chat-msg.ai').last()).toContainText('Tensor', { timeout: 20_000 });
    await page.waitForTimeout(1200);
    await page.evaluate(() => {
      const first = document.querySelector('.chat-msg.ai .chat-msg__txt');
      first.__kept = true;
      const range = document.createRange();
      range.selectNodeContents(first);
      const sel = window.getSelection();
      sel.removeAllRanges();
      sel.addRange(range);
    });
    await expect(page.locator('#chat-send')).not.toHaveAttribute('data-generating', 'true', { timeout: 60_000 });
    await page.waitForTimeout(500);
    expect(await page.evaluate(() => ({
      kept: document.querySelector('.chat-msg.ai .chat-msg__txt').__kept === true,
      selected: window.getSelection().toString().length,
    }))).toMatchObject({ kept: true });
    const copyX = (msg) => msg.locator('[data-act="copy"]').evaluate((e) => Math.round(e.getBoundingClientRect().left - e.closest('.chat-msg').getBoundingClientRect().left));
    const ai = page.locator('.chat-msg.ai');
    expect(await copyX(ai.first())).toBe(await copyX(ai.last()));
  });

  test('send and stop buttons keep a full-size glyph', async ({ page }) => {
    await openChat(page);
    await page.locator('#chat-new').click();
    const glyph = () => page.locator('#chat-send svg').first().evaluate((el) => {
      const r = el.getBoundingClientRect();
      return { w: Math.round(r.width), h: Math.round(r.height) };
    });
    expect(await glyph()).toEqual({ w: 14, h: 14 });
    await page.locator('#chat-input textarea').fill('Odpowiedz powoli.');
    await page.locator('#chat-send').click();
    await expect(page.locator('#chat-send')).toHaveAttribute('data-generating', 'true');
    expect(await glyph()).toEqual({ w: 14, h: 14 });
    await expect(page.locator('#chat-send use')).toHaveAttribute('href', '#i-stop');
  });

  test('a failed resume keeps the fragment, also after a reload', async ({ page }) => {
    const now = Date.now();
    await openChat(page, {
      seed: [{
        id: 'c-cut', title: 'Przerwana', createdAt: now, updatedAt: now, mode: 'text',
        messages: [
          { id: 1, role: 'user', text: 'AWARIA po fragmencie', ts: now },
          { id: 2, role: 'assistant', text: 'Zachowany fragment odpowiedzi', ts: now, failed: { kind: 'interrupted' } },
        ],
      }],
    });
    const answer = () => page.locator('.chat-msg.ai').last();
    await page.locator('.conv-item', { hasText: 'Przerwana' }).click();
    await expect(answer().locator('[data-act="resume"]')).toBeVisible();
    await answer().locator('[data-act="resume"]').click();
    await expect(answer().locator('.chat-msg__error')).toContainText('Nie udało się dokończyć', { timeout: 30_000 });
    await expect(answer().locator('.chat-msg__txt')).toContainText('Zachowany fragment odpowiedzi');
    await page.reload();
    await page.locator('.conv-item', { hasText: 'Przerwana' }).click();
    await expect(answer().locator('.chat-msg__txt')).toContainText('Zachowany fragment odpowiedzi');
  });

  test('empty bar shows a bare dash for TTFT and no info icon; copy confirms with a toast', async ({ page, context }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await openChat(page);
    await page.locator('#chat-new').click();
    await expect(page.locator('#m-ttft')).toHaveText('—');
    await expect(page.locator('#m-ttft-unit')).toBeHidden();
    await expect(page.locator('#chat-decode .chat-metric__info')).toBeHidden();
    await sendAndWaitDone(page, 'Odpowiedz powoli.');
    await expect(page.locator('#chat-decode .chat-metric__info')).toBeVisible();
    await page.locator('.chat-msg.ai').last().hover();
    await page.locator('.chat-msg.ai').last().locator('[data-act="copy"]').click();
    await expect(page.locator('.toast-container .toast')).toContainText('Skopiowano');
  });

  test('composer: Enter sends, Shift+Enter adds a line, counter, attachment chip', async ({ page }) => {
    const errors = await openChat(page);
    await page.locator('#chat-new').click();
    const ta = page.locator('#chat-input textarea');
    await ta.fill('linia 1');
    await ta.press('Shift+Enter');
    await ta.type('linia 2');
    expect(await ta.inputValue()).toBe('linia 1\nlinia 2');
    await expect(page.locator('#chat-input-counter')).not.toBeEmpty();

    const file = path.join(WORK_DIR, 'notatka.txt');
    fs.writeFileSync(file, 'treść załącznika');
    await page.locator('#chat-attach input[type="file"]').setInputFiles(file);
    await expect(page.locator('#chat-attachments')).toContainText('notatka.txt');

    await ta.press('Enter');
    await expect(page.locator('.chat-msg.user').last()).toContainText('linia 2');
    await expect(page.locator('#chat-send')).not.toHaveAttribute('data-generating', 'true', { timeout: 60_000 });
    expect(errors).toEqual([]);
  });
});
