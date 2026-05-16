// =============================================================================
// File: tests/e2e/helpers/spawn.js
// Description: Reusable helpers to spawn the tentaflow binary for UI e2e
//              tests. Encapsulates binary path checks, server boot wait, and
//              graceful teardown. Built on the pattern from mesh-pairing.spec.js
//              but specialised for single-node UI tests.
// =============================================================================

const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';

const BINARY = path.join(__dirname, '../../../tentaflow/target/release/tentaflow');
const DEFAULT_PORT = 18099;
const DEFAULT_DB = '/tmp/e2e-ui-test.db';
const CONFIG_TEMPLATE = path.join(__dirname, '../config-ui-test.toml');

function binaryExists() {
  return fs.existsSync(BINARY);
}

function baseUrl(port = DEFAULT_PORT) {
  return `https://127.0.0.1:${port}`;
}

function removeDbFiles(db) {
  for (const suffix of ['', '-wal', '-shm']) {
    try { fs.unlinkSync(db + suffix); } catch {}
  }
}

// Produces a config file at `outPath` derived from the template with the
// default port (18099) rewritten to `port`. Allows running multiple UI
// suites in parallel without colliding on ports or sqlite databases.
function renderConfig(outPath, port) {
  const tpl = fs.readFileSync(CONFIG_TEMPLATE, 'utf8');
  // Replace bind addresses "0.0.0.0:18099" and `port = 18099` mesh line.
  const rendered = tpl
    .replace(/"0\.0\.0\.0:18099"/g, `"0.0.0.0:${port}"`)
    .replace(/^port = 18099$/m, `port = ${port}`);
  fs.writeFileSync(outPath, rendered);
  return outPath;
}

function registerCleanup(child) {
  const cleanup = () => {
    try { if (child && !child.killed) child.kill('SIGTERM'); } catch {}
  };
  process.on('exit', cleanup);
  process.on('SIGINT', cleanup);
  process.on('SIGTERM', cleanup);
  process.on('uncaughtException', cleanup);
}

function startBinary({ port = DEFAULT_PORT, configFile, db = DEFAULT_DB } = {}) {
  removeDbFiles(db);
  let cfg = configFile;
  if (!cfg) {
    cfg = `/tmp/e2e-ui-config-${port}.toml`;
    renderConfig(cfg, port);
  }
  const proc = spawn(BINARY, ['-c', cfg, '--db', db], {
    env: { ...process.env, RUST_LOG: 'warn' },
  });
  proc.stderr.on('data', (d) => process.stderr.write(`[ui:${port}] ${d}`));
  registerCleanup(proc);
  return proc;
}

async function waitForServer(port = DEFAULT_PORT, maxWaitMs = 30000) {
  const start = Date.now();
  while (Date.now() - start < maxWaitMs) {
    try {
      const res = await fetch(`${baseUrl(port)}/api/auth/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username: 'admin', password: 'admin' }),
      });
      if ([200, 401, 403].includes(res.status)) return;
    } catch { /* not up yet */ }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`Server did not come up on port ${port} within ${maxWaitMs}ms`);
}

function stopBinary(proc) {
  if (proc && !proc.killed) {
    proc.kill('SIGTERM');
  }
}

module.exports = {
  BINARY,
  DEFAULT_PORT,
  DEFAULT_DB,
  CONFIG_TEMPLATE,
  binaryExists,
  baseUrl,
  renderConfig,
  registerCleanup,
  startBinary,
  waitForServer,
  stopBinary,
};
