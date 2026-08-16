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

// Cargo uses a shared target dir for all crates (.cargo/config.toml:
// target-dir = "target_shared" at repo root). Prefer release, accept debug.
const BINARY_CANDIDATES = [
  path.join(__dirname, '../../../target_shared/release/tentaflow'),
  path.join(__dirname, '../../../target_shared/debug/tentaflow'),
];
// `TENTAFLOW_E2E_BINARY` wins so a run can target a freshly built debug binary
// without touching a release artifact someone else produced.
const BINARY = process.env.TENTAFLOW_E2E_BINARY
  ?? BINARY_CANDIDATES.find((p) => fs.existsSync(p))
  ?? BINARY_CANDIDATES[0];

// `www/` is compiled INTO the binary (tentaflow-core/build.rs → wwwroot_embed.rs),
// so a binary older than the dashboard sources serves stale JS. That failure is
// brutal to read — the served codec simply lacks the encoder a spec asks for and
// the error names neither the file nor the reason. Warn loudly instead.
function warnIfDashboardIsStale() {
  try {
    const www = path.join(__dirname, '../../../tentaflow-core/www');
    const binMtime = fs.statSync(BINARY).mtimeMs;
    let newest = 0;
    const walk = (dir) => {
      for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
        const full = path.join(dir, entry.name);
        if (entry.isDirectory()) walk(full);
        // build.rs regenerates the wasm glue INTO www/, so those files are
        // always newer than the binary that just embedded them.
        else if (!/wasm_glue|\/generated\//.test(full)) {
          newest = Math.max(newest, fs.statSync(full).mtimeMs);
        }
      }
    };
    walk(www);
    if (newest > binMtime + 60_000) {
      const age = Math.round((newest - binMtime) / 60000);
      console.warn(
        `[e2e] WARNING: ${BINARY} is ~${age} min older than the files in www/. ` +
        'The dashboard is embedded in the binary, so the served front end will be stale. ' +
        'Rebuild (cd tentaflow && cargo build) or point TENTAFLOW_E2E_BINARY at a fresh one.',
      );
    }
  } catch { /* diagnostics only — never fail a run on this */ }
}
warnIfDashboardIsStale();
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

function startBinary({ port = DEFAULT_PORT, configFile, db = DEFAULT_DB, rustLog = 'warn' } = {}) {
  removeDbFiles(db);
  let cfg = configFile;
  if (!cfg) {
    cfg = `/tmp/e2e-ui-config-${port}.toml`;
    renderConfig(cfg, port);
  }
  const proc = spawn(BINARY, ['-c', cfg, '--db', db], {
    env: { ...process.env, RUST_LOG: rustLog },
  });
  // Keep an in-memory tail of backend logs so specs can attach them to
  // failure diagnostics (e.g. find_connection / PanelOpen traces).
  proc.logTail = [];
  const capture = (d) => {
    process.stderr.write(`[ui:${port}] ${d}`);
    proc.logTail.push(d.toString());
    if (proc.logTail.length > 500) proc.logTail.splice(0, proc.logTail.length - 500);
  };
  proc.stderr.on('data', capture);
  proc.stdout.on('data', capture);
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
