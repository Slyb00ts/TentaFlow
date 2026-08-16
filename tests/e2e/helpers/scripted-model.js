// =============================================================================
// File: tests/e2e/helpers/scripted-model.js
// Description: Zero-dep OpenAI-compatible endpoint that plays a FIXED script of
//              tool calls, so an agent turn is deterministic in CI. It exists to
//              exercise the Code Studio harness end to end — flow engine →
//              llm block → tool_exec → core.* tools → PEP → patch set → git —
//              without depending on a real model being reachable or on a real
//              model deciding to do the same thing twice.
//
//              What is NOT faked: everything downstream of the tool call. The
//              file is really written through the tool layer, the PEP really
//              gates it and git really commits it.
//
// Example:
//   const model = startScriptedModel({ script: helloWorldScript('hello.py') });
//   // ... point an `openai-compatible` service at model.baseUrl ...
//   model.stop();
// =============================================================================

const http = require('http');

// ---------------------------------------------------------------------------
// Scripts
// ---------------------------------------------------------------------------

// One turn of the agent loop. `tool` produces an assistant message carrying a
// tool call; `say` produces a plain assistant message, which is what ends the
// loop (the region stops on the first assistant turn without tool calls).
function tool(name, args) {
  return { kind: 'tool', name, args };
}
function say(text) {
  return { kind: 'say', text };
}

// The canonical "write a program" script: look around, write the file, run it,
// then answer. Four turns, so the loop iterates and the UI shows real rows.
function helloWorldScript({ path = 'hello.py', message = 'Witaj z Code Studio' } = {}) {
  const content = [
    '#!/usr/bin/env python3',
    '"""Napisane przez agenta Code Studio w teście e2e."""',
    '',
    '',
    'def main() -> None:',
    `    print("${message}")`,
    '',
    '',
    'if __name__ == "__main__":',
    '    main()',
    '',
  ].join('\n');

  return [
    tool('core.workspace_info', {}),
    // expected_sha256 = "" asserts the file does not exist yet, which is the
    // contract for a create (builtins.rs, CoreToolName::FsWrite).
    tool('core.fs_write', { path, content, expected_sha256: '' }),
    tool('core.exec', { argv: ['python3', path] }),
    // Asking for a commit is what MATERIALISES the patch set: `core.git_commit`
    // without an accepted review opens the review and parks (that contract is
    // spelled out in the orchestrator's own system prompt). Writing a file does
    // not create one on its own — the agent decides when the work is reviewable.
    tool('core.git_commit', {
      message: 'feat: add hello.py\n\nWritten by the Code Studio agent in an e2e run.',
    }),
    say(
      `Napisałem \`${path}\` i uruchomiłem go — wypisuje „${message}". ` +
        'Zmiana czeka na Twój przegląd.',
    ),
  ];
}

// Pulls run ids out of the tool results already in the conversation. The spawn
// result is a `tool` message; rather than depend on its exact shape, take every
// UUID it mentions — in this scenario the only ones present are the children's.
function runIdsFromConversation(request) {
  const ids = [];
  const uuid = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi;
  for (const m of request?.messages ?? []) {
    if (m.role !== 'tool') continue;
    const text = typeof m.content === 'string' ? m.content : JSON.stringify(m.content ?? '');
    for (const hit of text.match(uuid) ?? []) if (!ids.includes(hit)) ids.push(hit);
  }
  return ids;
}

// Delegation scenario. The orchestrator hands the work to a specialist, checks
// on it, waits for it, then answers — which is the whole "does it run and
// control subagents" question. Keyed by role because the parent and the child
// are two different agents hitting the same endpoint.
function delegationScripts({ path = 'from_subagent.py', message = 'Napisane przez subagenta' } = {}) {
  const content = [
    '#!/usr/bin/env python3',
    `print("${message}")`,
    '',
  ].join('\n');

  return [
    {
      // NOT a tool name: the agent-builder prompt embeds the live tool
      // catalogue, so `core.agent_spawn` appears there too and stole this
      // route. Match on prose that only this agent's prompt contains.
      match: 'Jesteś agentem programistycznym',
      steps: [
        tool('core.workspace_info', {}),
        tool('core.agent_spawn', {
          agent_name: 'code-implementer',
          task: `Utwórz plik ${path}, który wypisuje „${message}".`,
        }),
        // Can the parent see what the child is doing?
        tool('core.agent_list', {}),
        // Can it wait for it? The ids come from the spawn result that the
        // harness fed back into the conversation as a `tool` message.
        tool('core.agent_wait', (req) => ({
          run_ids: runIdsFromConversation(req),
          timeout_secs: 120,
        })),
        say('Subagent skończył, plik jest w drzewie roboczym.'),
      ],
    },
    {
      // Marker taken from the real request (dumped in the spec): the builder's
      // system prompt opens with this sentence. Matching the JSON contract text
      // was guesswork and did not hold.
      match: 'asystentem tworzenia agentów',
      steps: [
        // Deliberately wrapped in prose AND a code fence: the handler extracts
        // the first balanced {...} block precisely because models do this.
        say(
          'Jasne, proponuję takiego agenta:\n\n```json\n' +
          JSON.stringify({
            reply: 'Proponuję agenta do przeglądu bezpieczeństwa kodu.',
            proposal: {
              name: 'security-reviewer',
              display_name: 'Recenzent bezpieczeństwa',
              description: 'Przegląda zmiany w kodzie pod kątem podatności.',
              system_prompt: 'Jesteś recenzentem bezpieczeństwa. Czytasz kod i wskazujesz podatności.',
              tools: ['core.skill_view'],
              max_iterations: 25,
            },
          }) +
          '\n```\n\nDaj znać, czy dodać mu narzędzia sieciowe.',
        ),
      ],
    },
    {
      // code-implementer's system prompt opens with this sentence.
      match: 'Piszesz kod',
      steps: [
        tool('core.fs_write', { path, content, expected_sha256: '' }),
        say(`Utworzyłem ${path}.`),
      ],
    },
  ];
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

function chunkOf(id, model, delta, finishReason) {
  return `data: ${JSON.stringify({
    id,
    object: 'chat.completion.chunk',
    created: Math.floor(Date.now() / 1000),
    model,
    choices: [{ index: 0, delta, finish_reason: finishReason ?? null }],
  })}\n\n`;
}

function bodyFor(step, id, model, request) {
  if (step.kind === 'tool') {
    // `args` may be a function so a step can depend on what came back earlier —
    // core.agent_wait needs the run ids that core.agent_spawn just minted.
    const args = typeof step.args === 'function' ? step.args(request) : step.args;
    return {
      message: {
        role: 'assistant',
        content: null,
        tool_calls: [
          {
            id: `call_${id}`,
            type: 'function',
            function: { name: step.name, arguments: JSON.stringify(args) },
          },
        ],
      },
      finish_reason: 'tool_calls',
    };
  }
  return {
    message: { role: 'assistant', content: step.text },
    finish_reason: 'stop',
  };
}

/**
 * Starts the scripted endpoint.
 *
 * @param {object}   opts
 * @param {Array}    opts.script      steps produced by `tool()` / `say()`
 * @param {string}   opts.modelId     id advertised on /v1/models
 * @param {number}   opts.port        0 = ephemeral (read `.port` after start)
 * @returns {{port:number, baseUrl:string, calls:Array, stop:Function}}
 */
function startScriptedModel({ script, scripts, modelId = 'harness-test', port = 0 } = {}) {
  // Either one flat script, or several keyed by a marker in the system prompt so
  // a parent agent and its child can be driven independently.
  const routed = scripts ?? (script ? [{ match: null, steps: script }] : null);
  if (!Array.isArray(routed) || routed.length === 0) {
    throw new Error('scripted-model: pass `script` or `scripts`');
  }
  const cursors = new Map(routed.map((r, i) => [i, 0]));

  // Every prompt the harness sent, so a spec can assert what the agent saw
  // (system prompt, tool catalogue, tool results fed back into the loop).
  const calls = [];
  let cursor = 0;

  const server = http.createServer((req, res) => {
    const url = req.url.split('?')[0];

    // Health / discovery — the external provider probes this.
    if (req.method === 'GET' && (url === '/v1/models' || url === '/models')) {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ object: 'list', data: [{ id: modelId, object: 'model' }] }));
      return;
    }

    if (req.method === 'POST' && (url === '/v1/chat/completions' || url === '/chat/completions')) {
      let raw = '';
      req.on('data', (c) => { raw += c; });
      req.on('end', () => {
        let parsed = {};
        try { parsed = JSON.parse(raw || '{}'); } catch { /* keep {} */ }
        calls.push(parsed);

        // Route on the prompt. Prefer the system role, but fall back to the whole
        // conversation: not every caller puts its instructions in a system
        // message (the agent builder passes its contract differently).
        const textOf = (m) => (typeof m.content === 'string' ? m.content : JSON.stringify(m.content ?? ''));
        const systemText = (parsed.messages ?? []).filter((m) => m.role === 'system').map(textOf).join('\n');
        const allText = (parsed.messages ?? []).map(textOf).join('\n');
        let idx = routed.findIndex((r) => r.match && systemText.includes(r.match));
        if (idx < 0) idx = routed.findIndex((r) => r.match && allText.includes(r.match));
        if (idx < 0) idx = routed.findIndex((r) => !r.match);
        if (idx < 0) idx = 0;

        const steps = routed[idx].steps;
        const at = cursors.get(idx) ?? 0;
        if (process.env.SCRIPTED_MODEL_DEBUG) {
          console.log(`[scripted] route idx=${idx} match=${JSON.stringify(routed[idx].match)} `
            + `cursor=${at}/${steps.length} systemLen=${systemText.length}`);
        }
        // Past the end the model just stops, so a harness bug that loops forever
        // fails as a timeout instead of hanging the CI box.
        const step = at < steps.length ? steps[at] : say('Skrypt testowy się skończył.');
        cursors.set(idx, at + 1);
        cursor += 1;

        const id = `cmpl-${cursor}`;
        const { message, finish_reason: finish } = bodyFor(step, cursor, modelId, parsed);

        if (parsed.stream) {
          res.writeHead(200, {
            'content-type': 'text/event-stream',
            'cache-control': 'no-cache',
            connection: 'keep-alive',
          });
          if (message.tool_calls) {
            res.write(chunkOf(id, modelId, { role: 'assistant', tool_calls: message.tool_calls }));
          } else {
            res.write(chunkOf(id, modelId, { role: 'assistant', content: message.content }));
          }
          res.write(chunkOf(id, modelId, {}, finish));
          res.write('data: [DONE]\n\n');
          res.end();
          return;
        }

        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({
          id,
          object: 'chat.completion',
          created: Math.floor(Date.now() / 1000),
          model: modelId,
          choices: [{ index: 0, message, finish_reason: finish }],
          usage: { prompt_tokens: 512, completion_tokens: 64, total_tokens: 576 },
        }));
      });
      return;
    }

    res.writeHead(404, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ error: { message: `no route for ${req.method} ${url}` } }));
  });

  server.listen(port, '127.0.0.1');

  return {
    get port() { return server.address()?.port; },
    get baseUrl() { return `http://127.0.0.1:${server.address()?.port}/v1`; },
    get calls() { return calls; },
    get consumed() { return cursor; },
    stop() { try { server.close(); } catch { /* already down */ } },
  };
}

module.exports = { startScriptedModel, helloWorldScript, delegationScripts, tool, say };
