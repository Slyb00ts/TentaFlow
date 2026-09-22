// =============================================================================
// File: tests/e2e/helpers/stand-in-cli.js
// Description: Puts a stand-in vendor CLI where a spawned node resolves the
//              `codex` engine, so a spec can drive an agent turn that really
//              runs — through the bridge, the macOS process sandbox and the
//              workspace database — on a machine with no vendor CLI and no
//              provider account.
//
//              WHAT IT IS NOT. Nothing in the product is stubbed, and no
//              production code has a test branch: the node reaches this program
//              exactly the way it reaches the vendor's, through the PATH Core
//              hands the bridge and the `installation-complete` marker every
//              managed-CLI deploy writes. The contract it fills is the product's
//              own: `<cache>/coding-agents/<engine>/<version>/{installation-complete,
//              bin/<exe>}` (`services/deploy/managed_cli.rs`) plus the
//              `agent_runtime_nodes` / `agent_runtime_engines` rows that say
//              which node may hold credentials and that the engine is installed
//              there (`provider_accounts/repository.rs::list_runtime_nodes`).
//
//              COMPOSABLE WITH `helpers/spawn.js`, NOT A FORK OF IT. The cache
//              tree and the binary are placed before the node boots and the two
//              engine rows are written into the node's own database after
//              `waitForServer`, because they are read on demand at delegation
//              time. The spec keeps calling `startBinary` itself.
//
//              Ordering caveat that is a FACT about the product, not this file:
//              the stand-in must be compiled and placed BEFORE the node needs
//              it, and the rows written before the first delegation. Neither is
//              cached at boot, but a node that resolved "no runtime node" once
//              has already parked the run that asked.
// =============================================================================

const fs = require('fs');
const path = require('path');
const os = require('os');
const { execFileSync } = require('child_process');

const SOURCE = path.join(__dirname, 'stand-in-cli.c');

/// The version the fixture publishes. It is a plain string in the product's own
/// format (`validate_version`: first byte alphanumeric, the rest alphanumeric or
/// `.`/`-`/`_`) because the number itself is not verified against anything — the
/// node compares it with the `installation-complete` marker and the engine row,
/// never with the vendor's release list.
const VERSION = '0.155.0';

/// The engine the fixture stands in for. Codex on purpose, and not a preference:
/// it is the ONE managed CLI whose sign-in writes the credential FILE itself
/// (`codex login --device-auth`). Claude Code's `claude setup-token` prints a
/// token that the bridge lifts out of the terminal, which a stand-in cannot
/// reproduce without pretending to be the bridge as well.
const ENGINE = 'codex';
const EXECUTABLE = 'codex';

/// The credential the stand-in writes and the probe accepts. Kept here as well
/// as in the C source so a spec can seed an account on a node without walking
/// the sign-in — the two must stay byte-identical, and `credentialMatchesCli`
/// is what makes that a check rather than a comment.
function credentialJson() {
  return `${JSON.stringify({
    tokens: {
      account_id: 'stand-in-account',
      access_token: 'stand-in-access-token',
      refresh_token: 'stand-in-refresh-token',
    },
    last_refresh: '2026-01-01T00:00:00Z',
  })}\n`;
}

/// Compiles the stand-in for this machine. `-O2` is irrelevant to correctness
/// and kept only so the binary is the shape a real CLI has; the file is small
/// enough that a debug build would work the same.
function compile(destination) {
  const staging = `${destination}.build-${process.pid}`;
  try {
    execFileSync('clang', ['-O2', '-o', staging, SOURCE], { stdio: ['ignore', 'pipe', 'pipe'] });
    fs.renameSync(staging, destination);
  } catch (error) {
    try { fs.unlinkSync(staging); } catch { /* nothing to clean */ }
    throw new Error(`the stand-in CLI did not compile: ${error.stderr ?? error.message}`);
  }
  // 0555 — readable and executable by everyone, writable by nobody. The sandbox
  // grants read access to the resolved parents of the executables on PATH, and a
  // mode the bridge refuses to exec would surface as an opaque spawn failure.
  fs.chmodSync(destination, 0o555);
}

function sqlite(db, query) {
  const out = execFileSync(
    '/usr/bin/sqlite3',
    ['-json', '-cmd', '.timeout 5000', db, query],
    { encoding: 'utf8' },
  ).trim();
  return out ? JSON.parse(out) : [];
}

function sqlQuote(value) {
  return `'${String(value).replace(/'/g, "''")}'`;
}

/// Installs the stand-in for `home`'s node and records it in `db`.
///
/// Returns the handle a spec asserts against: the paths the node will resolve,
/// the account-root helpers the credential and the sandbox records live under,
/// and a wait for the process record that proves the sandbox really wrapped the
/// child.
///
/// `home` is the SAME directory `startBinary({ home })` sets `TENTAFLOW_HOME` to,
/// which is what makes `<home>/cache` the cache root and `<home>/keys` the key
/// root (`paths.rs::cache_dir`/`keys_dir`).
function installStandInCli({ db, home, version = VERSION }) {
  if (!db) throw new Error('installStandInCli needs the node database path');
  if (!home) throw new Error('installStandInCli needs the node home (TENTAFLOW_HOME)');

  const installRoot = path.join(home, 'cache', 'coding-agents', ENGINE, version);
  const binDir = path.join(installRoot, 'bin');
  const executable = path.join(binDir, EXECUTABLE);
  fs.mkdirSync(binDir, { recursive: true });
  compile(executable);
  // The marker's CONTENT is the version, compared with the version asked for —
  // a file that exists but says something else is an installation of an
  // different release, and the node refuses it (`managed_cli.rs::installation`).
  fs.writeFileSync(path.join(installRoot, 'installation-complete'), version);

  // Which node this is. Written at boot by the sync runtime
  // (`sync/runtime.rs::ensure_local_node_in_sync_identity`) from the mesh
  // signing key, and it is the id every runtime-node row must carry — a row
  // under any other id is a row about a machine that is not this one.
  const rows = sqlite(db, "SELECT value FROM settings WHERE key = 'sync_local_node_id'");
  const nodeId = rows[0]?.value ?? '';
  if (!nodeId) {
    throw new Error(
      'the node has no sync_local_node_id yet; install the stand-in after waitForServer()',
    );
  }

  // Parent first: `agent_runtime_engines` references it ON DELETE CASCADE, so
  // replacing the node row afterwards would silently delete the engine row.
  sqlite(db, `INSERT OR REPLACE INTO agent_runtime_nodes (node_id, receives_accounts, updated_by) \
VALUES (${sqlQuote(nodeId)}, 1, 'e2e-stand-in')`);
  sqlite(db, `INSERT OR REPLACE INTO agent_runtime_engines \
(node_id, engine_id, install_state, version, installed_at) \
VALUES (${sqlQuote(nodeId)}, ${sqlQuote(ENGINE)}, 'installed', ${sqlQuote(version)}, \
strftime('%Y-%m-%dT%H:%M:%SZ','now'))`);

  /// `<home>/keys/coding-agents/accounts/<account_id>` — the account's own root
  /// (`services/coding_agent.rs::account_root`). The id must be a canonical
  /// UUID: the product parses it and refuses anything that does not round-trip.
  const accountRoot = (accountId) => path.join(
    home, 'keys', 'coding-agents', 'accounts', String(accountId),
  );

  /// The node's own record of a process the sandbox wrapped
  /// (`coding-agent-bridge/src/process.rs`): `<kind>-<pid>.json` under the
  /// account's `processes/`, carrying `supervisor_root` — the temporary
  /// directory the macOS supervisor holds for as long as the child lives. It is
  /// DELETED when the child ends, so a spec must read it while the turn runs.
  const processRecords = (accountId) => {
    const dir = path.join(accountRoot(accountId), 'processes');
    let names = [];
    try { names = fs.readdirSync(dir); } catch { return []; }
    return names
      .filter((name) => name.endsWith('.json'))
      .map((name) => {
        try { return JSON.parse(fs.readFileSync(path.join(dir, name), 'utf8')); } catch { return null; }
      })
      .filter(Boolean);
  };

  /// The first record of `kind`, waited for — an `expect.poll`-friendly shape.
  /// Returns the record or `null`; a spec asserts on `supervisor_root` itself
  /// rather than on the mere existence of a file, because "a process ran" and
  /// "the sandbox wrapped it" are different claims.
  const waitForProcessRecord = async (accountId, kind, {
    timeoutMs = 120_000, intervalMs = 100,
  } = {}) => {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const found = processRecords(accountId).find((record) => record.kind === kind);
      if (found) return found;
      await new Promise((resolve) => setTimeout(resolve, intervalMs));
    }
    return null;
  };

  /// Every process record this account writes from now on, collected as it
  /// appears. `waitForProcessRecord` answers about ONE moment; a record is
  /// DELETED when its child ends, so a child that lives for a second is
  /// invisible to a caller that only starts looking afterwards. This watches
  /// until `stop()`, which is what makes a proof about a process that already
  /// ended possible at all.
  ///
  /// Polling rather than `fs.watch`: the directory need not exist yet, the
  /// number of files in it is tiny, and a watch on a directory the bridge
  /// creates later would have to be re-armed anyway.
  const watchProcessRecords = (accountId, { intervalMs = 50 } = {}) => {
    const seen = new Map();
    const scan = () => {
      for (const record of processRecords(accountId)) {
        if (record?.id) seen.set(record.id, record);
      }
    };
    scan();
    const timer = setInterval(scan, intervalMs);
    return {
      /// The records collected so far, oldest first.
      records: () => [...seen.values()],
      /// The first record of `kind`, or null.
      find: (kind) => [...seen.values()].find((record) => record.kind === kind) ?? null,
      /// Every kind seen, for a failure message that says what DID run.
      kinds: () => [...new Set([...seen.values()].map((record) => record.kind))],
      stop: () => {
        clearInterval(timer);
        scan();
      },
    };
  };

  /// Writes the account's credential the way a finished sign-in leaves it: the
  /// canonical `<account_root>/credentials/codex/auth.json`, which is what
  /// `agent_runtime::read_bridge_credential` reads and what the bridge
  /// materializes into a session profile. The sign-in path goes through the
  /// login home and the bridge's own publication; this is the same file with
  /// the same bytes, for specs whose subject is not the sign-in.
  const seedCredential = (accountId) => {
    const dir = path.join(accountRoot(accountId), 'credentials', ENGINE);
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
    const file = path.join(dir, 'auth.json');
    fs.writeFileSync(file, credentialJson(), { mode: 0o600 });
    return file;
  };

  /// The account id of a personal account for this engine, read from the node's
  /// own database. A spec that created one through the console needs its id to
  /// name the account root, and reading it back from the row the server wrote is
  /// what makes the two the same account.
  const accountIdForUser = () => {
    const rows = sqlite(db, `SELECT account_id FROM provider_accounts \
WHERE engine_id = ${sqlQuote(ENGINE)} AND scope = 'user' ORDER BY created_at DESC LIMIT 1`);
    return rows[0]?.account_id ?? '';
  };

  return {
    engine: ENGINE,
    version,
    nodeId,
    installRoot,
    binDir,
    executable,
    accountRoot,
    accountIdForUser,
    processRecords,
    waitForProcessRecord,
    watchProcessRecords,
    seedCredential,
  };
}

/// Whether a file the stand-in wrote is the credential this module says it is.
/// Exists so the C constant and the JS one cannot drift apart unnoticed.
function credentialMatchesCli(file) {
  return fs.readFileSync(file, 'utf8') === credentialJson();
}

/// A scratch directory for a spec that needs one outside the node's home —
/// the temporary root the sign-in terminal is watched from, for instance.
function scratchDir(prefix = 'stand-in-cli-') {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

module.exports = {
  installStandInCli,
  credentialJson,
  credentialMatchesCli,
  scratchDir,
  VERSION,
  ENGINE,
  EXECUTABLE,
};
