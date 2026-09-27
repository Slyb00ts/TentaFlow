// =============================================================================
// File: tests/e2e/code-studio-live-progress.spec.js
// Description: A program built and then changed in Code Studio over two turns,
//              through the WHOLE pinned harness — orchestrator, planner, critic,
//              implementer, tester, critic, review — while checking what the
//              operator sees DURING the turn, not only after it.
//
//              The model is scripted but slow: every answer "thinks" for a few
//              seconds, the way a real model does. An instant script can never
//              show whether the screen says what is happening between two
//              events, and that gap is where an operator used to see nothing
//              (or "idle") while the agent was working.
//
//              Turn 1 writes a snake game's logic with its tests and runs them.
//              Turn 2 adds a score and levels to the SAME files, after reading
//              them, and runs the tests again. Both turns end in an accepted
//              review, and the worktree holds what the agent wrote.
// =============================================================================

const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { startScriptedModel, tool, say } = require('./helpers/scripted-model');
const {
  ensureCodeStudioApp, registerModelProvider, createWorkspace, openSession, drainApprovals,
  sessionRuns, acceptOpenReviews, readWorktreeFile,
} = require('./helpers/code-studio');

const PORT = 18131;
const DB = '/tmp/e2e-code-studio-live.db';
const WORKSPACE = `snake-${Date.now().toString(36)}`;
// How long every model answer takes. Long enough that the screen must say
// something between events, short enough that a two-turn pipeline stays fast.
const THINK_MS = 4_000;
const TURN_TIMEOUT = 5 * 60_000;

const GAME_V1 = `"""Logika gry Snake — bez grafiki, żeby dało się ją testować."""
from dataclasses import dataclass, field

DIRS = {"up": (0, -1), "down": (0, 1), "left": (-1, 0), "right": (1, 0)}


@dataclass
class Game:
    width: int = 10
    height: int = 10
    snake: list = field(default_factory=lambda: [(5, 5), (4, 5), (3, 5)])
    direction: str = "right"
    food: tuple = (7, 5)
    alive: bool = True

    def step(self):
        if not self.alive:
            return
        dx, dy = DIRS[self.direction]
        head = (self.snake[0][0] + dx, self.snake[0][1] + dy)
        inside = 0 <= head[0] < self.width and 0 <= head[1] < self.height
        if not inside or head in self.snake:
            self.alive = False
            return
        self.snake.insert(0, head)
        if head == self.food:
            self.food = self._next_food()
        else:
            self.snake.pop()

    def _next_food(self):
        for y in range(self.height):
            for x in range(self.width):
                if (x, y) not in self.snake:
                    return (x, y)
        return None
`;

const TESTS_V1 = `from snake import Game


def test_moves_right():
    g = Game()
    g.step()
    assert g.snake[0] == (6, 5) and len(g.snake) == 3


def test_eats_and_grows():
    g = Game()
    g.step()
    g.step()
    assert len(g.snake) == 4


def test_wall_kills():
    g = Game(width=6)
    g.step()
    assert not g.alive


if __name__ == "__main__":
    tests = [fn for name, fn in sorted(globals().items()) if name.startswith("test_")]
    for fn in tests:
        fn()
    print(f"snake: wszystkie testy przeszły ({len(tests)})")
`;

const GAME_V2 = GAME_V1
  .replace('    alive: bool = True\n', '    alive: bool = True\n    score: int = 0\n')
  .replace(
    '        if head == self.food:\n            self.food = self._next_food()\n',
    '        if head == self.food:\n            self.score += 10\n            self.food = self._next_food()\n',
  )
  .replace(
    '    def _next_food(self):',
    '    @property\n    def level(self):\n        return 1 + self.score // 50\n\n    def _next_food(self):',
  );

const TESTS_V2 = TESTS_V1.replace(
  '\n\nif __name__ == "__main__":',
  `

def test_score_counts_food():
    g = Game()
    g.step()
    g.step()
    assert g.score == 10 and g.level == 1


if __name__ == "__main__":`,
);

const ANSWER_1 = 'Gotowe: snake.py z logiką gry i test_snake.py — 3 testy przechodzą.';
const ANSWER_2 = 'Dodałem punkty (10 za jedzenie) i poziomy co 50 punktów; 4 testy przechodzą.';

/// The `blob_id` the agent read for `path`, copied from the fs_read result the
/// harness fed back — exactly what a real model does.
function readBlobId(request, path) {
  for (const m of [...(request?.messages ?? [])].reverse()) {
    if (m.role !== 'tool') continue;
    const text = typeof m.content === 'string' ? m.content : JSON.stringify(m.content ?? '');
    if (!text.includes(`"path":"${path}"`)) continue;
    const hit = text.match(/"blob_id"\s*:\s*"([0-9a-f]+)"/);
    if (hit) return hit[1];
  }
  throw new Error(`no fs_read result for ${path} in the conversation`);
}

function orchestratorSteps() {
  return [
    // Turn 1 — create.
    tool('core.workspace_info', {}),
    tool('core.fs_write', { path: 'snake.py', content: GAME_V1, expected_blob_id: '' }),
    tool('core.fs_write', { path: 'test_snake.py', content: TESTS_V1, expected_blob_id: '' }),
    tool('core.exec', { argv: ['python3', 'test_snake.py'] }),
    say(ANSWER_1),
    // Turn 2 — change what turn 1 wrote.
    tool('core.fs_read', { path: 'snake.py' }),
    tool('core.fs_write', (req) => ({
      path: 'snake.py', content: GAME_V2, expected_blob_id: readBlobId(req, 'snake.py'),
    })),
    tool('core.fs_read', { path: 'test_snake.py' }),
    tool('core.fs_write', (req) => ({
      path: 'test_snake.py', content: TESTS_V2, expected_blob_id: readBlobId(req, 'test_snake.py'),
    })),
    tool('core.exec', { argv: ['python3', 'test_snake.py'] }),
    say(ANSWER_2),
  ];
}

/// The pinned pipeline's other agents, two turns each. `task_plan` replaces the
/// plan, so every turn's plan numbers from 1 and the implementer closes task 1.
function pipelineScripts() {
  return [
    {
      match: 'Jesteś planistą zmian w kodzie',
      steps: [
        tool('core.task_plan', { tasks: [{ title: 'Logika gry Snake z testami', detail: 'test_snake.py przechodzi' }] }),
        say('Plan: jedno zadanie.'),
        tool('core.task_plan', { tasks: [{ title: 'Punkty i poziomy', detail: 'test_score_counts_food przechodzi' }] }),
        say('Plan: jedno zadanie.'),
      ],
    },
    { match: 'Jestes krytykiem', repeat: true, steps: [say('Sprawdziłem — BEZ UWAG.')] },
    {
      match: 'Piszesz kod',
      steps: [
        tool('core.task_list', {}),
        tool('core.task_update', { ordinal: 1, status: 'done' }),
        say('Zadanie zrobione w turze orkiestratora — odhaczyłem je.'),
        tool('core.task_list', {}),
        tool('core.task_update', { ordinal: 1, status: 'done' }),
        say('Zadanie zrobione w turze orkiestratora — odhaczyłem je.'),
      ],
    },
    {
      match: 'Uruchamiasz testy i buildy',
      steps: [say('Testy przechodzą.'), say('Testy przechodzą.')],
    },
  ];
}

let proc;
let model;
let workspaceId = '';
let sessionId = '';
const scope = () => ({ workspaceId, sessionId });

test.describe.configure({ mode: 'serial' });

test.beforeAll(async () => {
  if (!binaryExists()) test.skip(true, 'tentaflow binary not built');
  model = startScriptedModel({
    delayMs: THINK_MS,
    scripts: [
      { match: 'Jesteś agentem programistycznym', steps: orchestratorSteps() },
      ...pipelineScripts(),
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

/// Sends one message from the composer, the way an operator does.
async function send(page, text) {
  const composer = page.locator('#cs-session-view textarea').first();
  await composer.fill(text);
  await composer.press('Enter');
}

/// What the "now" line under the chat says at this moment.
/// Empty when the widget has hidden itself because nothing runs.
async function nowLine(page) {
  const line = page.locator('[data-activity="now"] .tf-aa-line').first();
  if (!(await line.count())) return '';
  return ((await line.textContent({ timeout: 1_000 }).catch(() => '')) ?? '').trim();
}

/// What the latest `core.exec` call printed, read the way an operator reads it:
/// the result is folded into the tool row and opens with a click on it.
async function execOutputOnScreen(page) {
  const stream = page.locator('#cs-session-view .cs-stream[data-stream="console"]');
  // The call's row folds the tool result (stdout included); the `Exec` event
  // beside it carries only the working directory.
  const row = stream.locator('.ev-tool[data-detail-text*="stdout"]').last();
  await row.scrollIntoViewIfNeeded();
  await row.click();
  const detail = stream.locator('.ev-detail').last();
  await expect(detail).toBeVisible();
  return (await detail.textContent()) ?? '';
}

/// The root run of the n-th message. `ordinal` numbers EVERY run of the
/// session, sub-agents included, so a turn is found by its position among roots.
async function rootOfTurn(page, turn) {
  const roots = (await sessionRuns(page, scope())).filter((r) => r.kind === 'root');
  return roots.sort((a, b) => (a.ordinal ?? 0) - (b.ordinal ?? 0))[turn - 1];
}

/// Drives a turn to its end the way an attentive operator does: answers every
/// approval and accepts the review the pipeline stops at.
async function settleTurn(page, turn) {
  let acceptedAt = 0;
  await expect.poll(async () => {
    await drainApprovals(page, scope());
    if ((await acceptOpenReviews(page, scope())).length && !acceptedAt) acceptedAt = Date.now();
    return (await rootOfTurn(page, turn))?.status ?? 'missing';
  }, { timeout: TURN_TIMEOUT, intervals: [1000], message: `turn ${turn} never finished` })
    .toMatch(/completed|failed/);
  // Accepting in the Changes pane IS the review's answer: the run that waited
  // on it must end within a model answer or two, not at the review timeout.
  expect(acceptedAt, `turn ${turn} ended without a review to accept`).toBeGreaterThan(0);
  expect(Date.now() - acceptedAt, 'the run kept waiting after the review was accepted')
    .toBeLessThan(6 * THINK_MS);
}

/// Checks the screen while a turn is in flight: within one model answer of the
/// message the line under the chat must name the run as working, its clock must
/// move, and a tool call must be on screen while its run is still going. The
/// stream shows a tool without its `core.` prefix.
async function assertVisibleWhileRunning(page, turn, firstTool) {
  // The first answer takes THINK_MS; before it lands the only true statement is
  // "the model is working". "idle", or nothing at all, is the defect.
  await expect.poll(() => nowLine(page), {
    timeout: THINK_MS - 500, intervals: [250],
    message: 'nothing on screen says the agent is working while the model thinks',
  }).toMatch(/(myśli…|thinking…) · \d+s/);
  const secondsOf = (line) => Number((line.match(/ (\d+)s$/) ?? [])[1] ?? -1);
  const early = secondsOf(await nowLine(page));
  await page.waitForTimeout(2_200);
  const later = await nowLine(page);
  // Either the clock moved, or the turn moved on to a tool — both show life.
  if (/myśli…|thinking…/.test(later)) {
    expect(secondsOf(later), `the clock froze: "${later}"`).toBeGreaterThan(early);
  }

  const stream = page.locator('#cs-session-view .cs-stream[data-stream="console"]');
  await expect(stream.getByText(firstTool, { exact: false }).first())
    .toBeVisible({ timeout: 3 * THINK_MS });
  const root = await rootOfTurn(page, turn);
  expect(root?.status, 'the tool row appeared only after the turn had ended')
    .toMatch(/running|waiting/);
}

/// When the agent asks for consent, the operator must still see the
/// conversation the question is about — the card may not eat the stream.
async function assertConversationVisibleWhileAsking(page) {
  const card = page.locator('#cs-session-view [data-answer]:not([hidden])');
  await expect(card, 'no consent card was raised for the write').toBeVisible({ timeout: 3 * THINK_MS });
  const stream = page.locator('#cs-session-view .cs-stream[data-stream="console"]');
  const box = await stream.boundingBox();
  const viewport = page.viewportSize();
  expect.soft(box?.height ?? 0, 'the question card squeezed the conversation out of view')
    .toBeGreaterThanOrEqual(0.2 * viewport.height);
}

test.describe('Code Studio — program budowany w dwóch turach, widoczny na żywo', () => {
  test('przygotowanie: model i workspace', async ({ page }) => {
    await loginAsAdmin(page, { port: PORT });
    await ensureCodeStudioApp(page);
    await registerModelProvider(page, model);
    // `trusted_native`: the default process sandbox needs unprivileged user
    // namespaces, which a stock Ubuntu 24.04 host denies to bwrap. This suite
    // is about the harness and what the screen shows, not about the sandbox.
    workspaceId = await createWorkspace(page, PORT, WORKSPACE, { execMode: 'trusted_native' });
  });

  test('tura 1: agent pisze grę i testy, a ekran pokazuje, co trwa', async ({ page }) => {
    test.setTimeout(TURN_TIMEOUT + 60_000);
    await loginAsAdmin(page, { port: PORT });
    sessionId = await openSession(page, PORT, { workspaceName: WORKSPACE, workspaceId });

    await send(page, 'Napisz logikę gry Snake w Pythonie z testami i uruchom testy.');
    await assertVisibleWhileRunning(page, 1, 'workspace_info');
    await assertConversationVisibleWhileAsking(page);

    // The pipeline's sub-agents are listed while the turn is still going.
    await expect.poll(async () => {
      await drainApprovals(page, scope());
      const runs = await sessionRuns(page, scope());
      const root = await rootOfTurn(page, 1);
      const children = runs.filter((r) => r.kind === 'subagent' && r.parent_run_id === root?.run_id);
      return children.length > 0 && /running|waiting/.test(root?.status ?? '');
    }, { timeout: TURN_TIMEOUT, intervals: [1000], message: 'no sub-agent was visible while the turn ran' })
      .toBe(true);
    await expect(page.locator('[data-activity="dock"]')).toContainText('code-planner', { timeout: 30_000 });

    await settleTurn(page, 1);
    const root = await rootOfTurn(page, 1);
    expect(root.status, `turn 1 ended ${root.status}`).toBe('completed');

    const view = page.locator('#cs-session-view');
    // The review card in the stream follows the decision instead of saying
    // "undecided" about files the operator already accepted.
    await expect(view.locator('[data-patch-card] .pf .n').first())
      .toHaveText(/accepted|zaakceptowan/i, { timeout: 15_000 });
    // The tests really ran: python's stdout came back through core.exec.
    expect(await execOutputOnScreen(page)).toContain('wszystkie testy przeszły (3)');
    // What the agent answered is on screen — not only that its run ended.
    await expect(view.getByText(ANSWER_1, { exact: false }).first()).toBeVisible({ timeout: 15_000 });
    expect(await readWorktreeFile(page, scope(), 'snake.py')).toBe(GAME_V1);
    expect(await readWorktreeFile(page, scope(), 'test_snake.py')).toBe(TESTS_V1);
    await expect.poll(() => nowLine(page), { timeout: 10_000 }).toBe('');
  });

  test('tura 2: agent czyta swoje pliki, dodaje punkty i poziomy, testy znów przechodzą', async ({ page }) => {
    test.setTimeout(TURN_TIMEOUT + 60_000);
    await loginAsAdmin(page, { port: PORT });
    await openSession(page, PORT, { workspaceName: WORKSPACE, workspaceId });

    await send(page, 'Dodaj licznik punktów i poziomy trudności, z testem.');
    await assertVisibleWhileRunning(page, 2, 'fs_read');

    await settleTurn(page, 2);
    const root = await rootOfTurn(page, 2);
    expect(root.status, `turn 2 ended ${root.status}`).toBe('completed');

    const view = page.locator('#cs-session-view');
    expect(await execOutputOnScreen(page)).toContain('wszystkie testy przeszły (4)');
    await expect(view.getByText(ANSWER_2, { exact: false }).first()).toBeVisible({ timeout: 15_000 });
    const game = await readWorktreeFile(page, scope(), 'snake.py');
    expect(game).toBe(GAME_V2);
    expect(game).toContain('self.score += 10');
    expect(await readWorktreeFile(page, scope(), 'test_snake.py')).toBe(TESTS_V2);
  });

  test('model pracował w pętli: katalog narzędzi i wyniki wróciły do rozmowy', async () => {
    const orchestrator = model.calls.filter((c) => (c.messages ?? [])
      .some((m) => m.role === 'system' && String(m.content ?? '').includes('Jesteś agentem programistycznym')));
    expect(orchestrator.length, 'the orchestrator did not iterate over both turns').toBeGreaterThanOrEqual(11);
    const last = orchestrator.at(-1);
    expect((last.messages ?? []).map((m) => m.role)).toContain('tool');
    // The second turn saw the first: the conversation carries turn 1's answer.
    expect(JSON.stringify(last.messages)).toContain(ANSWER_1);
  });
});
