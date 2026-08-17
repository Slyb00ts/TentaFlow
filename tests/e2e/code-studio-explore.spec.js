// =============================================================================
// File: tests/e2e/code-studio-explore.spec.js
// Description: A WALK through Code Studio rather than an assertion about one
//              feature. It drives the app the way a person does — click the
//              thing that is visible, type an ordinary sentence, open every
//              panel, shrink the window to a phone — and writes down everything
//              that misbehaves on the way.
//
//              The difference from the other Code Studio specs matters: those
//              prove a named contract and stop at the first violation. This one
//              keeps going and reports the WHOLE list, because a run that
//              surfaces eight defects is worth more than eight runs that each
//              surface one.
//
//              Nothing here pokes the protocol to make a screen appear. If a
//              panel can only be reached by clicking, it gets clicked, so a
//              defect that lives in the UI layer cannot hide behind a working
//              backend.
// =============================================================================

const { test, expect } = require('@playwright/test');
const {
  startBinary, stopBinary, waitForServer, binaryExists, baseUrl,
} = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const {
  startScriptedModel, helloWorldScript, enforcedPipelineScripts,
} = require('./helpers/scripted-model');

const PORT = 18114;
const DB = '/tmp/e2e-code-studio-explore.db';
const WORKSPACE = `spacer-${Date.now().toString(36)}`;

let proc;
let model;

// Everything that went wrong, collected instead of thrown. Printed as one block
// at the end so a single run yields a full defect list.
const findings = [];
function note(where, what) {
  findings.push(`${where} — ${what}`);
  console.log(`  [znalezione] ${where} — ${what}`);
}

test.describe.configure({ mode: 'serial' });

test.beforeAll(async () => {
  if (!binaryExists()) test.skip(true, 'tentaflow binary not built');
  model = startScriptedModel({
    scripts: [
      { match: 'Jesteś agentem programistycznym', steps: helloWorldScript() },
      ...enforcedPipelineScripts(),
    ],
  });
  proc = startBinary({ port: PORT, db: DB, rustLog: process.env.RUST_LOG ?? 'warn' });
  await waitForServer(PORT);
});

test.afterAll(async () => {
  stopBinary(proc);
  model?.stop();
  await new Promise((r) => setTimeout(r, 1500));
});

// Time-boxes one interaction. A step that hangs becomes a FINDING with the time
// it burned, instead of eating the whole test budget and hiding everything the
// walk would have found afterwards.
async function step(label, fn, budgetMs = 15_000) {
  const started = Date.now();
  try {
    const out = await Promise.race([
      fn(),
      new Promise((_, reject) => setTimeout(() => reject(new Error('przekroczony czas')), budgetMs)),
    ]);
    const took = Date.now() - started;
    if (took > budgetMs * 0.6) note(label, `dziala, ale wolno (${(took / 1000).toFixed(1)} s)`);
    return out;
  } catch (e) {
    note(label, `${String(e.message ?? e).slice(0, 160)} po ${((Date.now() - started) / 1000).toFixed(1)} s`);
    return null;
  }
}

async function api(page, action, payload) {
  return page.evaluate(async ([a, p]) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return p === null ? ApiBinary.one(a) : ApiBinary.action(a, p);
  }, [action, payload === undefined ? null : payload]);
}

// Attaches the listeners that turn a silent breakage into a finding. A page
// that renders but logs a TypeError is broken; the user just cannot see it yet.
function watch(page, label) {
  page.on('console', (msg) => {
    if (msg.type() !== 'error') return;
    const text = msg.text();
    // A failed favicon or an aborted stream on teardown is noise, not a defect.
    if (/favicon|net::ERR_ABORTED|Failed to load resource/i.test(text)) return;
    note(label, `blad w konsoli: ${text.slice(0, 240)}`);
  });
  page.on('pageerror', (err) => {
    note(label, `nieobsluzony wyjatek: ${String(err).slice(0, 240)}`);
  });
  page.on('requestfailed', (req) => {
    const url = req.url();
    if (/favicon/.test(url)) return;
    note(label, `zadanie nieudane: ${req.method()} ${url.slice(0, 120)} (${req.failure()?.errorText})`);
  });
}

async function registerProvider(page) {
  const nodes = await api(page, 'meshNodeListRequest');
  const first = (nodes?.nodes ?? [])[0] ?? {};
  await api(page, 'serviceManifestDeployRequest', {
    engineId: 'openai-compatible',
    deployMethod: 'external',
    nodeId: first.nodeId ?? first.node_id ?? '',
    configJson: JSON.stringify({
      base_url: model.baseUrl,
      api_key: 'explore-key',
      auth_mode: 'api',
      model_repo: 'harness-test',
    }),
  });
}

test.describe('Spacer po Code Studio', () => {
  test('wchodzi, zaklada workspace i otwiera sesje', async ({ page }) => {
    test.setTimeout(420_000);
    watch(page, 'start');
    await step('logowanie', () => loginAsAdmin(page, { port: PORT }), 60_000);
    await step('rejestracja dostawcy modelu', () => registerProvider(page), 60_000);

    const me = await api(page, 'authMeRequest');
    const users = await api(page, 'usersListRequest');
    const userId = (users?.users ?? [])
      .find((u) => (u.username ?? '') === (me?.username ?? 'admin'))?.id;
    await api(page, 'codeStudioWorkspaceCreatorGrantSetRequest', { userId, granted: true });

    // Deep link, the way someone pastes a URL from a colleague.
    await step('wejscie z adresu #/code-studio', async () => {
      await page.goto(`${baseUrl(PORT)}/#/code-studio`);
      await page.locator('#cs-new, #cs-empty-new, #cs-table-host').first().waitFor({ timeout: 25_000 });
    }, 30_000);

    // The empty state should tell a first-time user what to do.
    const emptyText = await page.locator('#cs-list-view').innerText().catch(() => '');
    if (emptyText && emptyText.trim().length < 10) {
      note('lista pusta', 'pusty ekran nie tlumaczy, co zrobic dalej');
    }

    await step('otwarcie kreatora', () => page.locator('#cs-new, #cs-empty-new').first().click({ timeout: 10_000 }));
    await step('wpisanie nazwy', () => page.locator('#cs-wz-name input').first().fill(WORKSPACE));

    // Walk the wizard forward by pressing whatever button is offered, reporting
    // what each step showed. The count is not hard-coded: the walk stops when
    // the footer stops offering a way forward.
    for (let n = 1; n <= 6; n += 1) {
      const next = page.locator('[data-action="next"]:visible').first();
      if (await next.count().catch(() => 0) === 0) break;
      const label = (await next.innerText().catch(() => '')).trim();
      // What the step actually asks for — printed so a wizard that silently
      // refuses to advance can be read rather than guessed at.
      const shown = (await page.locator('tf-modal:visible, .cs-wizard:visible, [role="dialog"]:visible')
        .first().innerText().catch(() => '')).replace(/\s+/g, ' ').trim().slice(0, 220);
      console.log(`  [kreator] krok ${n}: przycisk "${label}" | ekran: ${shown}`);
      const moved = await step(`kreator krok ${n} ("${label}")`,
        () => next.click({ timeout: 10_000 }), 12_000);
      if (moved === null) break;
      await page.waitForTimeout(400);
      const err = await page.locator('#cs-wz-error:visible').innerText().catch(() => '');
      if (err.trim()) note('kreator', `krok ${n} zglosil blad: ${err.trim().slice(0, 160)}`);
    }

    const reached = await step('workspace staje sie aktywny', async () => {
      const deadline = Date.now() + 150_000;
      for (;;) {
        const body = await api(page, 'codeStudioWorkspacesListRequest', {});
        const ws = (body?.workspaces ?? []).find((w) => w.name === WORKSPACE);
        if (ws?.status === 'active') return ws.status;
        if (Date.now() > deadline) return ws?.status ?? 'brak';
        await page.waitForTimeout(2000);
      }
    }, 160_000);
    if (reached !== 'active') {
      note('kreator', `workspace zatrzymal sie na stanie "${reached}" zamiast "active"`);
    }
  });

  test('rozmawia z agentem i oglada wszystkie panele', async ({ page }) => {
    test.setTimeout(300_000);
    watch(page, 'sesja');
    await loginAsAdmin(page, { port: PORT });
    await page.goto(`${baseUrl(PORT)}/#/code-studio`);

    await page.locator(`text=${WORKSPACE}`).first().click();
    await page.locator('#cs-open-session:not([disabled])').waitFor({ timeout: 90_000 });
    await page.locator('#cs-open-session').click();
    await page.locator('#cs-sess-title input').first().fill('spacer');
    await page.locator('[data-action="create"]').first().click();
    await page.waitForSelector('#cs-session-view', { state: 'visible', timeout: 60_000 });

    // An ordinary sentence, the way someone actually asks for something. The
    // composer is a `tf-textarea`, and it is painted after the session view
    // becomes visible — waiting for the view alone is a race.
    const box = page.locator('#cs-session-view textarea').first();
    const ready = await step('pole wiadomosci pojawia sie',
      () => box.waitFor({ state: 'visible', timeout: 20_000 }), 25_000);
    if (ready === null) {
      note('sesja', 'pole do napisania wiadomosci nie pojawilo sie po otwarciu sesji');
    } else {
      await step('napisanie prosby', () => box.fill(
        'Napisz prosty program w Pythonie, ktory wypisze krotki tekst, i uruchom go.',
      ));
      await step('wyslanie', () => box.press('Enter'));
    }

    // Wait for the turn the way a person does: watch the console until the
    // agent's own words appear, rather than sleeping a fixed number of seconds.
    // The agent stops and ASKS before it touches anything — answering is part
    // of using the app, so the walk answers. An earlier version waited on the
    // word "powitanie", which is a word from the request itself, so it saw the
    // user's own message echoed back, walked on while the run was still parked
    // on an unanswered question, and then blamed the panels for being empty.
    let asked = 0;
    let lastQuestion = '';
    let repeats = 0;
    let lastActivity = Date.now();
    const answered = await step('agent pracuje, a my odpowiadamy na pytania', async () => {
      const deadline = Date.now() + 150_000;
      for (;;) {
        // Once the agent has stopped asking and stopped producing output, the
        // turn has settled — parked on the review gate, most likely. Sitting out
        // the full deadline would only report the walk's own patience as a
        // defect.
        if (asked > 0 && Date.now() - lastActivity > 30_000) return 'zaparkowana';
        // The agent asks permission before it touches anything, and answering
        // is part of using the app — so the walk answers.
        const ask = page.locator('[data-answer]:not([hidden])').first();
        if (await ask.count().catch(() => 0) > 0) {
          const question = (await ask.innerText().catch(() => '')).replace(/\s+/g, ' ').slice(0, 110);

          // Asking for a commit parks the run on a REVIEW: the commit is built
          // from blobs a human accepted. That gate is where the agent's turn
          // legitimately ends, so record it rather than treating it as a stall.
          if (/git_commit/i.test(question)) {
            console.log('  [tura] agent doszedl do bramki przegladu — czeka na akceptacje zmian');
            return 'review-gate';
          }

          const option = ask.locator('tf-option-row[data-action]:not([disabled])').first();
          if (await option.count().catch(() => 0) > 0) {
            // A question that comes back after it was answered means the click
            // was swallowed. Say so once instead of hammering the same button.
            if (question === lastQuestion) {
              repeats += 1;
              if (repeats === 3) {
                note('pytanie', `to samo pytanie wraca mimo odpowiedzi — klikniecie ginie: ${question}`);
              }
              if (repeats >= 3) { await page.waitForTimeout(2000); continue; }
            } else {
              repeats = 0;
              lastQuestion = question;
            }
            console.log(`  [pytanie ${asked + 1}] ${question}`);
            const clicked = await option.click({ timeout: 6000 })
              .then(() => true)
              .catch((e) => { note('pytanie', `nie da sie kliknac odpowiedzi: ${String(e.message ?? e).slice(0, 120)}`); return false; });
            if (!clicked) { await page.waitForTimeout(2000); continue; }
            asked += 1;
            lastActivity = Date.now();
            await page.waitForTimeout(1500);
            continue;
          }
          note('pytanie', `agent pyta, ale nie widac zadnej odpowiedzi do klikniecia: ${question}`);
        }

        const text = await page.locator('#cs-session-view').innerText().catch(() => '');
        // The program's OUTPUT is the only thing that proves the whole chain ran.
        if (/Witaj z Code Studio/i.test(text)) return text;
        if (Date.now() > deadline) return null;
        await page.waitForTimeout(2000);
      }
    }, 170_000);
    console.log(`  [podsumowanie tury] pytan: ${asked}`);
    // A turn that ends parked on the review gate is a COMPLETE turn: the agent
    // wrote the file, ran it, and is waiting for a human to accept the change.
    // What has to be true either way is that it actually did the work.
    if (asked === 0) {
      note('sesja', 'agent nie poprosil o ani jedna zgode — tura w ogole nie ruszyla');
    }

    // The point of the console is that you can SEE what the agent did. If the
    // tool calls are invisible, the operator is trusting a black box.
    const shown = (await page.locator('#cs-session-view').innerText().catch(() => '')).toLowerCase();
    // The console must SHOW what the agent did — an operator who cannot see the
    // tool calls is trusting a black box.
    for (const trace of ['fs_write', 'hello.py']) {
      if (!shown.includes(trace)) {
        note('konsola', `konsola nie pokazuje sladu po "${trace}" — nie widac, co agent zrobil`);
      }
    }

    // Every dock panel, opened by clicking its TAB — the dock is a `tf-tabs`
    // component, so the pane body is inert until the tab switches it.
    for (const pane of ['zmiany', 'pliki', 'git', 'terminal', 'agenci']) {
      const tab = page.locator(`tf-tab[panel="cs-dock-pane-${pane}"]`).first();
      if (await tab.count().catch(() => 0) === 0) {
        note('panele', `nie ma zakladki panelu "${pane}"`);
        continue;
      }
      const ok = await step(`panel "${pane}"`, () => tab.click({ timeout: 8000 }), 10_000);
      if (ok === null) continue;
      await page.waitForTimeout(1200);
      const body = (await page.locator(`#cs-dock-pane-${pane}`).innerText().catch(() => '')).trim();
      if (!body) note('panele', `panel "${pane}" jest calkiem pusty po otwarciu`);
      console.log(`  [panel ${pane}] ${body.replace(/\s+/g, ' ').slice(0, 160)}`);
      // The file the agent wrote has to be findable where a person looks for
      // it: in the file tree and among the pending changes.
      // The tree opens collapsed at the workspace root — expand it the way a
      // person does before deciding the file is missing.
      if (pane === 'pliki') {
        const root = page.locator(`#cs-dock-pane-${pane} [data-node], #cs-dock-pane-${pane} .cs-tree-row`).first();
        if (await root.count().catch(() => 0) > 0) {
          await root.click({ timeout: 5000 }).catch(() => {});
          await page.waitForTimeout(1500);
        }
      }
      const body2 = (await page.locator(`#cs-dock-pane-${pane}`).innerText().catch(() => '')).trim();
      if (pane === 'pliki') console.log(`  [drzewo po rozwinieciu] ${body2.replace(/\s+/g, ' ').slice(0, 200)}`);
      if (pane === 'pliki' && !/hello\.py/i.test(body2)) {
        note('panele', 'drzewo plikow nie pokazuje pliku, ktory agent wlasnie utworzyl');
      }
      if (pane === 'zmiany' && !/hello\.py/i.test(body)) {
        note('panele', 'panel zmian nie pokazuje pliku, ktory agent wlasnie utworzyl');
      }
    }

    // Getting OUT of a session has to be possible by clicking. Try each control
    // the UI offers; if none of them is reachable, that is the finding.
    let left = false;
    for (const sel of ['.cs-stage-exit', '#cs-mtop-exit', '#cs-back']) {
      const c = page.locator(`${sel}:visible`).first();
      if (await c.count().catch(() => 0) === 0) continue;
      const ok = await step(`wyjscie z sesji (${sel})`, () => c.click({ timeout: 6000 }), 8000);
      if (ok !== null) { left = true; break; }
    }
    if (!left) note('nawigacja', 'z otwartej sesji nie da sie wrocic zadnym widocznym przyciskiem');

    await page.waitForTimeout(1500);
    const row = (await page.locator('#cs-table-host, #cs-workspace-view').first()
      .innerText().catch(() => '')).trim();
    if (row && !new RegExp(WORKSPACE, 'i').test(row)) {
      note('nawigacja', 'po wyjsciu z sesji nie widac workspace u, w ktorym pracowalismy');
    }
  });

  test('dziala na telefonie', async ({ browser }) => {
    test.setTimeout(180_000);
    // A real phone, not just a narrow window: `hasTouch` is what makes
    // `pointer: coarse` match, and that is the rule tap targets are sized by.
    const context = await browser.newContext({
      viewport: { width: 390, height: 844 },
      hasTouch: true,
      isMobile: true,
      ignoreHTTPSErrors: true,
    });
    const page = await context.newPage();
    watch(page, 'telefon');
    await loginAsAdmin(page, { port: PORT });
    await page.goto(`${baseUrl(PORT)}/#/code-studio`);
    await page.waitForTimeout(2000);

    // The one thing that must never happen on a phone: sideways scrolling.
    const overflow = await page.evaluate(() => {
      const d = document.documentElement;
      return { scroll: d.scrollWidth, client: d.clientWidth };
    });
    if (overflow.scroll > overflow.client + 2) {
      note('telefon', `strona przewija sie w bok (${overflow.scroll} > ${overflow.client})`);
    }

    const visible = await page.locator(`text=${WORKSPACE}`).first().isVisible().catch(() => false);
    if (!visible) note('telefon', 'lista workspace ow nie pokazuje sie na waskim ekranie');

    // Tap targets under ~32 px are a miss-fest on a touch screen.
    const small = await page.evaluate(() => {
      const out = [];
      for (const el of document.querySelectorAll('tf-button, button, [data-action]')) {
        const r = el.getBoundingClientRect();
        if (r.width === 0 || r.height === 0) continue;
        if (r.height >= 32) continue;
        const name = (el.getAttribute('label') || el.textContent || '').replace(/\s+/g, ' ').trim();
        out.push(`${el.tagName.toLowerCase()}`
          + `${el.id ? `#${el.id}` : ''}`
          + `${el.className ? `.${String(el.className).split(' ')[0]}` : ''}`
          + ` "${name.slice(0, 30)}" h=${Math.round(r.height)}`);
      }
      return out.slice(0, 8);
    });
    for (const s of small) note('telefon', `za maly cel dotykowy: ${s}`);
    await context.close();
  });

  test('podsumowanie spaceru', async () => {
    if (findings.length) {
      console.log(`\n=== ${findings.length} rzeczy do poprawy ===`);
      findings.forEach((f, i) => console.log(`${i + 1}. ${f}`));
    }
    expect(findings, `spacer znalazl ${findings.length} rzeczy:\n${findings.join('\n')}`)
      .toEqual([]);
  });
});
