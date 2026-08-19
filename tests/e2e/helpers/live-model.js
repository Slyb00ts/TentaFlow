// =============================================================================
// File: tests/e2e/helpers/live-model.js
// Description: Recording pass-through proxy to a REAL OpenAI-compatible model
//              server (llama.cpp / vLLM). Drop-in replacement for
//              `scripted-model.js`: same `{port, baseUrl, calls, stop}` shape,
//              so a spec swaps one for the other without touching its
//              assertions.
//
//              Why a proxy instead of pointing the provider straight at the
//              model: the specs assert on `model.calls` — that the loop
//              iterated, that the tool catalogue reached the model, that tool
//              results were fed back. Those assertions are the evidence the
//              harness works. A proxy keeps them meaningful while EVERY
//              decision (which tool, which arguments, when to stop) comes from
//              the real model.
//
//              What is NOT faked here: nothing. The request is forwarded
//              verbatim except for `model`, which is rewritten to the name the
//              upstream actually serves — the provider is registered under a
//              test-local repo name that the model server has never heard of.
//
// Example:
//   const model = startLiveModel();          // reads TF_E2E_MODEL_* from env
//   // ... point an `openai-compatible` service at model.baseUrl ...
//   model.stop();
// =============================================================================

const http = require('http');
const https = require('https');
const { URL } = require('url');

/// Upstream config. `TF_E2E_MODEL_URL` is the OpenAI base (…/v1).
function upstreamConfig() {
  const base = process.env.TF_E2E_MODEL_URL;
  if (!base) return null;
  return {
    base: base.replace(/\/+$/, ''),
    key: process.env.TF_E2E_MODEL_KEY ?? '',
    model: process.env.TF_E2E_MODEL_NAME ?? '',
  };
}

/// True when the suite was asked to run against a real model.
function liveModelRequested() {
  return Boolean(process.env.TF_E2E_MODEL_URL);
}

/**
 * Starts the recording proxy.
 *
 * @returns {{port:number, baseUrl:string, calls:Array, stop:Function, live:boolean, upstream:object}}
 */
function startLiveModel() {
  const up = upstreamConfig();
  if (!up) throw new Error('startLiveModel: TF_E2E_MODEL_URL is not set');
  const target = new URL(up.base);
  const agentMod = target.protocol === 'https:' ? https : http;

  const calls = [];
  const debug = Boolean(process.env.LIVE_MODEL_DEBUG);
  const dumpPath = process.env.LIVE_MODEL_DUMP || '/tmp/live-model-calls.jsonl';
  try { require('fs').writeFileSync(dumpPath, ''); } catch { /* diagnostics only */ }
  console.log(`[live] recording every request to ${dumpPath}`);

  // A 27B at ~20 tok/s needs minutes for a long tool-heavy turn, so nothing
  // here may impose a short socket deadline. The spec's own timeout is the
  // only budget that matters.
  const REQUEST_TIMEOUT_MS = 15 * 60_000;

  function forward(req, res, rawBody, pathSuffix) {
    let parsed = {};
    try { parsed = JSON.parse(rawBody || '{}'); } catch { /* keep {} */ }

    // The provider is registered with a test-local repo name; the model server
    // serves its own alias. Rewrite so the upstream recognises the request.
    if (up.model) parsed.model = up.model;

    const outBody = JSON.stringify(parsed);
    calls.push(parsed);
    // Always capture exactly what the harness sent, one JSON per line. A chat
    // template that rejects a payload names neither the payload nor the
    // offending message, so holding the request is the only way to debug one.
    // Unconditional on purpose: a live run is slow and rare, and losing the
    // evidence costs another ten minutes.
    try {
      require('fs').appendFileSync(
        dumpPath,
        `${JSON.stringify({ at: new Date().toISOString(), body: parsed })}\n`,
      );
    } catch { /* diagnostics only */ }
    if (debug) {
      const tools = (parsed.tools ?? []).map((t) => t.function?.name).join(',');
      console.log(`[live] → ${parsed.messages?.length ?? 0} msgs, tools=[${tools}], stream=${!!parsed.stream}`);
    }

    const opts = {
      protocol: target.protocol,
      hostname: target.hostname,
      port: target.port || (target.protocol === 'https:' ? 443 : 80),
      path: `${target.pathname.replace(/\/+$/, '')}${pathSuffix}`,
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'content-length': Buffer.byteLength(outBody),
        ...(up.key ? { authorization: `Bearer ${up.key}` } : {}),
      },
      timeout: REQUEST_TIMEOUT_MS,
    };

    const upReq = agentMod.request(opts, (upRes) => {
      // Pass the upstream through verbatim, streaming included: the harness
      // consumes SSE deltas and the UI renders them as they arrive.
      res.writeHead(upRes.statusCode || 502, {
        'content-type': upRes.headers['content-type'] ?? 'application/json',
        ...(upRes.headers['transfer-encoding'] ? {} : {}),
      });
      upRes.pipe(res);
      if (debug) {
        let seen = 0;
        upRes.on('data', (c) => { seen += c.length; });
        upRes.on('end', () => console.log(`[live] ← ${upRes.statusCode}, ${seen} B`));
      }
    });

    upReq.setTimeout(REQUEST_TIMEOUT_MS, () => {
      upReq.destroy(new Error('upstream timeout'));
    });
    upReq.on('error', (err) => {
      console.error('[live] upstream error:', err.message);
      if (!res.headersSent) res.writeHead(502, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ error: { message: `live-model upstream: ${err.message}` } }));
    });
    upReq.end(outBody);
  }

  const server = http.createServer((req, res) => {
    const url = req.url.split('?')[0];

    // Discovery — the external provider probes this before it will route.
    // Answered locally under the name the provider was registered with, so the
    // probe does not depend on how the upstream labels its alias.
    if (req.method === 'GET' && (url === '/v1/models' || url === '/models')) {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({
        object: 'list',
        data: [{ id: up.model || 'live', object: 'model' }],
      }));
      return;
    }

    const isChat = url === '/v1/chat/completions' || url === '/chat/completions';
    const isCompletions = url === '/v1/completions' || url === '/completions';
    const isEmbeddings = url === '/v1/embeddings' || url === '/embeddings';
    if (req.method === 'POST' && (isChat || isCompletions || isEmbeddings)) {
      let raw = '';
      req.on('data', (c) => { raw += c; });
      req.on('end', () => {
        const suffix = isChat ? '/chat/completions' : (isCompletions ? '/completions' : '/embeddings');
        forward(req, res, raw, suffix);
      });
      return;
    }

    res.writeHead(404, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ error: { message: `live-model: no route ${req.method} ${url}` } }));
  });

  // Long turns must not be cut by the server's own idle timeout.
  server.timeout = REQUEST_TIMEOUT_MS;
  server.headersTimeout = REQUEST_TIMEOUT_MS;
  server.requestTimeout = REQUEST_TIMEOUT_MS;
  server.listen(0, '127.0.0.1');

  return {
    live: true,
    upstream: up,
    get port() { return server.address()?.port; },
    get baseUrl() { return `http://127.0.0.1:${server.address()?.port}/v1`; },
    get calls() { return calls; },
    stop() { try { server.close(); } catch { /* already down */ } },
  };
}

module.exports = { startLiveModel, liveModelRequested, upstreamConfig };
