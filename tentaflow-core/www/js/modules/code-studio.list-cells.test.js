// =============================================================================
// File: modules/code-studio.list-cells.test.js
// Description: Unit tests for the Code Studio workspace-list cells — `repoPath`,
//       `repoCell` and `nodeCell`. Two rules are pinned here because a reviewer
//       caught the UI breaking both: a workspace WITHOUT a repository must not
//       print a branch (it would name the branch of a thing that does not
//       exist), and the node caption must only appear when it separates rows.
//       The functions are not exported (the module pulls the whole dashboard in),
//       so their source is cut out of the real file by brace matching and
//       evaluated against stubs — the code under test is the shipped code.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'code-studio.js'), 'utf8');

function cut(src, name) {
  const start = src.indexOf(`function ${name}(`);
  if (start < 0) throw new Error(`no definition: ${name}`);
  let depth = 0;
  let i = src.indexOf('{', start);
  for (; i < src.length; i += 1) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  return src.slice(start, i + 1);
}

// The three cells share the module's escaping helpers, its translator and the
// node list; everything else they touch is the workspace record itself.
const PRELUDE = `
  const escapeHtml = (v) => String(v ?? '');
  const escapeAttr = (v) => String(v ?? '');
  const t = (key) => key;
  const state = { nodes: [] };
`;

function build(names) {
  const body = names.map((n) => cut(source, n)).join('\n');
  // eslint-disable-next-line no-new-func
  return new Function(`${PRELUDE}\n${body}\nreturn { ${names.join(', ')}, state };`)();
}

const { repoPath } = build(['repoPath']);
const repoApi = build(['repoPath', 'repoCell']);
const nodeApi = build(['nodeCell']);

// ---------------------------------------------------------------------------
// repoPath — what survives of a remote once the noise is gone
// ---------------------------------------------------------------------------

test('repoPath: an https remote loses its scheme and its .git suffix', () => {
  assert.equal(repoPath('https://github.com/serde-rs/serde.git'), 'github.com/serde-rs/serde');
});

test('repoPath: an scp-style ssh remote becomes one readable path', () => {
  assert.equal(repoPath('git@github.com:euvic/tentaflow.git'), 'github.com/euvic/tentaflow');
});

test('repoPath: an ssh:// remote keeps its host and path', () => {
  assert.equal(repoPath('ssh://git@gitlab.euvic.local/b2b/portal.git'), 'gitlab.euvic.local/b2b/portal');
});

test('repoPath: an explicit PORT is not mistaken for the ssh path separator', () => {
  assert.equal(repoPath('https://gitlab.euvic.local:8443/b2b/portal.git'),
    'gitlab.euvic.local:8443/b2b/portal');
});

test('repoPath: a remote without a suffix is left alone', () => {
  assert.equal(repoPath('https://github.com/octocat/Hello-World'), 'github.com/octocat/Hello-World');
});

// ---------------------------------------------------------------------------
// repoCell — the branch belongs to a repository
// ---------------------------------------------------------------------------

test('repoCell: a workspace WITHOUT a repository prints no branch at all', () => {
  const html = repoApi.repoCell({ repoUrl: '', targetBranch: 'main', defaultBranch: 'main' });
  assert.match(html, /repo_none/);
  assert.doesNotMatch(html, /main/);
  assert.doesNotMatch(html, /cell-title/);
});

test('repoCell: snake_case payloads take the same empty path', () => {
  const html = repoApi.repoCell({ repo_url: null, target_branch: 'main' });
  assert.match(html, /repo_none/);
  assert.doesNotMatch(html, /main/);
});

test('repoCell: a repository prints the path over its branch, both mono', () => {
  const html = repoApi.repoCell({
    repoUrl: 'https://github.com/serde-rs/serde.git', targetBranch: 'master',
  });
  assert.match(html, /tf-table__cell--mono/);
  assert.match(html, /cell-title">github\.com\/serde-rs\/serde</);
  assert.match(html, /cell-sub">master</);
});

test('repoCell: the untrimmed remote stays reachable as the tooltip', () => {
  const html = repoApi.repoCell({ repoUrl: 'https://github.com/serde-rs/serde.git' });
  assert.match(html, /title="https:\/\/github\.com\/serde-rs\/serde\.git"/);
});

test('repoCell: a repository without a branch prints one line, not an empty one', () => {
  const html = repoApi.repoCell({ repoUrl: 'https://github.com/octocat/Hello-World.git' });
  assert.match(html, /cell-title/);
  assert.doesNotMatch(html, /cell-sub/);
});

// ---------------------------------------------------------------------------
// nodeCell — a caption that repeats on every row is filler
// ---------------------------------------------------------------------------

test('nodeCell: one node in the mesh means the name carries the cell alone', () => {
  nodeApi.state.nodes = [{ node_id: 'a' }];
  const html = nodeApi.nodeCell({ nodeName: 'mainpc', isLocal: true });
  assert.match(html, /cell-title">mainpc</);
  assert.doesNotMatch(html, /cell-sub/);
});

test('nodeCell: with several nodes the caption separates rows, so it appears', () => {
  nodeApi.state.nodes = [{ node_id: 'a' }, { node_id: 'b' }];
  const html = nodeApi.nodeCell({ nodeName: 'mainpc', isLocal: true });
  assert.match(html, /cell-sub">node_local</);
});

test('nodeCell: a workspace on ANOTHER node always says so', () => {
  nodeApi.state.nodes = [{ node_id: 'a' }];
  const html = nodeApi.nodeCell({ node_name: 'gpu-01', is_local: false });
  assert.match(html, /cell-title">gpu-01</);
  assert.match(html, /cell-sub">node_remote</);
});
