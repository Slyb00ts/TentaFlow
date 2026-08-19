// =============================================================================
// File: components/tf-code-editor.tokenizers.test.js
// Description: Tests for the tf-code-editor tokenizer bank — the six languages
// added for Code Studio (Rust, C#, HTML, CSS, shell, TOML) plus regressions for
// the seven that shipped before. `tokenizeLine` is module-private (the module
// pulls DOM-only dependencies), so its real source region is sliced out of the
// component file and evaluated in isolation — the code under test is the code
// that ships.
// =============================================================================

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'tf-code-editor.js'), 'utf8');

// The tokenizer bank is one contiguous region: from WORD_RE (used by the
// Gherkin tokenizer) to the end of tokenizeLine.
function extractTokenizerBank(src) {
  const start = src.indexOf('const WORD_RE');
  if (start < 0) throw new Error('WORD_RE not found');
  const fnStart = src.indexOf('function tokenizeLine(', start);
  if (fnStart < 0) throw new Error('tokenizeLine not found');
  let depth = 0;
  let i = src.indexOf('{', fnStart);
  for (; i < src.length; i += 1) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  return src.slice(start, i + 1);
}

// eslint-disable-next-line no-new-func
const tokenizeLine = new Function(`${extractTokenizerBank(source)}\nreturn tokenizeLine;`)();

// ---- helpers ---------------------------------------------------------------

// Threads the per-line state exactly like the component does and flattens the
// result to {text, cls} spans.
function tokenize(lang, code) {
  const out = [];
  let state = '';
  for (const line of code.split('\n')) {
    const res = tokenizeLine(lang, line, state);
    state = res.state;
    const spans = [];
    for (let i = 0; i < res.toks.length; i += 3) {
      spans.push({ text: line.slice(res.toks[i], res.toks[i + 1]), cls: res.toks[i + 2] });
    }
    out.push({ line, spans, toks: res.toks });
  }
  return out;
}

function all(doc) { return doc.flatMap((l) => l.spans); }

function has(doc, cls, text) {
  return all(doc).some((t) => t.cls === cls && t.text === text);
}

function clsOf(doc, text) {
  const hit = all(doc).find((t) => t.text === text);
  return hit ? hit.cls : null;
}

// Every token must be a forward, non-overlapping range — the renderer paints
// them in order and silently drops anything that goes backwards.
function assertOrdered(doc, label) {
  for (const { line, toks } of doc) {
    let prevEnd = 0;
    for (let i = 0; i < toks.length; i += 3) {
      assert.ok(toks[i] >= prevEnd, `${label}: token starts before the previous end in "${line}"`);
      assert.ok(toks[i + 1] > toks[i], `${label}: empty/negative token in "${line}"`);
      assert.ok(toks[i + 1] <= line.length, `${label}: token past end of "${line}"`);
      prevEnd = toks[i + 1];
    }
  }
}

// ---- Rust ------------------------------------------------------------------

const RUST_SAMPLE = `// module entry
#[derive(Debug, Clone)]
pub struct Engine<'a> {
    name: &'a str,
    slots: Vec<u32>,
}

impl<'a> Engine<'a> {
    /// Doc comment
    pub fn run(&mut self, n: usize) -> Result<u32, Error> {
        let raw = r#"a "quoted" path\\here"#;
        println!("{} slots, raw={}", n, raw);
        /* nested /* block */ comment */
        let mask = 0xFF_u32 + 1_000;
        Ok(mask)
    }
}`;

test('rust: keywords, types, lifetimes, attributes, macros, raw strings', () => {
  const doc = tokenize('rust', RUST_SAMPLE);
  assertOrdered(doc, 'rust');

  assert.ok(has(doc, 'kw', 'pub'), 'pub is a keyword');
  assert.ok(has(doc, 'kw', 'struct'));
  assert.ok(has(doc, 'kw', 'impl'));
  assert.ok(has(doc, 'kw', 'let'));

  assert.ok(has(doc, 'typ', 'Engine'), 'capitalised idents are types');
  assert.ok(has(doc, 'typ', 'Vec'));
  assert.ok(has(doc, 'typ', 'u32'), 'primitives are types');
  assert.ok(has(doc, 'typ', 'usize'));

  assert.ok(has(doc, 'dec', "'a"), 'lifetimes are annotations, not char literals');
  assert.ok(has(doc, 'dec', '#[derive(Debug, Clone)]'), 'attribute spans the whole bracket');

  assert.ok(has(doc, 'fn', 'println!'), 'macro invocation includes the bang');
  assert.ok(has(doc, 'fn', 'run'), 'fn name after `fn`');

  assert.ok(has(doc, 'str', 'r#"a "quoted" path\\here"#'), 'raw string keeps its inner quotes');
  assert.ok(has(doc, 'com', '// module entry'));
  assert.ok(has(doc, 'com', '/// Doc comment'));
  assert.ok(has(doc, 'com', '/* nested /* block */ comment */'), 'nested block comment');
  assert.ok(has(doc, 'num', '0xFF_u32'));
  assert.ok(has(doc, 'num', '1_000'));
});

test('rust: a multi-line raw string keeps the rest of the file out of string colour', () => {
  const doc = tokenize('rust', ['let s = r#"open', 'still string', '"#;', 'let n = 42;'].join('\n'));
  assert.equal(clsOf([doc[1]], 'still string'), 'str');
  assert.ok(has([doc[3]], 'num', '42'), 'code after the raw string tokenizes normally');
});

test('rust: a char literal is not mistaken for a lifetime', () => {
  const doc = tokenize('rust', "let c = 'x';");
  assert.ok(has(doc, 'str', "'x'"));
});

// ---- C# --------------------------------------------------------------------

const CS_SAMPLE = `#region Api
using System.Text;

namespace Tenta.Api;

[ApiController]
public sealed class Handler
{
    private readonly string _path = @"C:\\repos\\tenta";

    public async Task<int> RunAsync(int count)
    {
        var msg = $"got {count} items from {_path}";
        /* block
           comment */
        return await Task.FromResult(count * 2);
    }
}`;

test('csharp: keywords, types, attributes, verbatim and interpolated strings', () => {
  const doc = tokenize('csharp', CS_SAMPLE);
  assertOrdered(doc, 'csharp');

  assert.ok(has(doc, 'kw', 'public'));
  assert.ok(has(doc, 'kw', 'sealed'));
  assert.ok(has(doc, 'kw', 'namespace'));
  assert.ok(has(doc, 'kw', 'await'));
  assert.ok(has(doc, 'typ', 'string'), 'primitive aliases are types');
  assert.ok(has(doc, 'typ', 'Handler'), 'type name after `class`');
  assert.ok(has(doc, 'typ', 'Task'));

  assert.ok(has(doc, 'dec', '#region Api'), 'preprocessor directive');
  assert.ok(has(doc, 'dec', '[ApiController]'), 'attribute');

  assert.ok(has(doc, 'str', '@"C:\\repos\\tenta"'), 'verbatim string ignores backslash escapes');
  assert.ok(has(doc, 'str', '$"got '), 'interpolated string breaks at the hole');
  assert.ok(has(doc, 'op', '{'), 'interpolation braces are operators');
  assert.ok(has(doc, 'com', '/* block'));
  assert.ok(has(doc, 'fn', 'RunAsync'));
});

test('csharp: a verbatim string may span lines', () => {
  const doc = tokenize('csharp', ['var s = @"line one', 'line two";', 'var n = 7;'].join('\n'));
  assert.equal(clsOf([doc[1]], 'line two"'), 'str');
  assert.ok(has([doc[2]], 'num', '7'));
});

// ---- HTML ------------------------------------------------------------------

const HTML_SAMPLE = `<!DOCTYPE html>
<!-- page shell -->
<div class="cs-shell" data-nav>
  <p>text &amp; more</p>
  <style>
    .cs-shell { color: #ff0; }
  </style>
  <script type="module">
    const n = 42;          // inline js
    export function boot() { return \`v\${n}\`; }
  </script>
</div>`;

test('html: tags, attributes, comments and embedded script/style', () => {
  const doc = tokenize('html', HTML_SAMPLE);
  assertOrdered(doc, 'html');

  assert.ok(has(doc, 'dec', '<!DOCTYPE html>'));
  assert.ok(has(doc, 'com', '<!-- page shell -->'));
  assert.ok(has(doc, 'kw', '<div'));
  assert.ok(has(doc, 'kw', '</div'));
  assert.ok(has(doc, 'prop', 'class'));
  assert.ok(has(doc, 'prop', 'data-nav'));
  assert.ok(has(doc, 'str', '"cs-shell"'));

  // The <style> body is CSS, the <script> body is JavaScript.
  assert.ok(has(doc, 'typ', '.cs-shell'), 'selector inside <style>');
  assert.ok(has(doc, 'prop', 'color'), 'declaration inside <style>');
  assert.ok(has(doc, 'num', '#ff0'));
  assert.ok(has(doc, 'kw', 'const'), 'keyword inside <script>');
  assert.ok(has(doc, 'kw', 'export'));
  assert.ok(has(doc, 'num', '42'));
  assert.ok(has(doc, 'com', '// inline js'));
});

test('html: a tag spanning several lines keeps its attribute state', () => {
  const doc = tokenize('html', ['<img', '  src="a.png"', '  alt="x">', 'tail'].join('\n'));
  assert.ok(has(doc, 'prop', 'src'));
  assert.ok(has(doc, 'str', '"a.png"'));
  assert.ok(has(doc, 'str', '"x"'));
  assert.equal(doc[3].spans.length, 0, 'plain text after the tag is not tokenized');
});

test('html: script content is not cut short by a string that looks like a tag', () => {
  const doc = tokenize('html', ['<script>', 'const s = "</div>";', '</script>'].join('\n'));
  assert.ok(has(doc, 'kw', 'const'));
  assert.ok(has(doc, 'kw', '</script'));
});

// ---- CSS -------------------------------------------------------------------

const CSS_SAMPLE = `@media (max-width: 900px) {
  .tf-diff__split, .recon > .diff:hover {
    grid-template-columns: 1fr;
    --tf-gap: 10px;
    color: rgba(34, 197, 94, 0.9) !important;
  }
}
/* trailing note */
[data-nav="1"] { background: #0a0e22; }`;

test('css: selectors, properties, at-rules, colours and comments', () => {
  const doc = tokenize('css', CSS_SAMPLE);
  assertOrdered(doc, 'css');

  assert.ok(has(doc, 'kw', '@media'));
  assert.ok(has(doc, 'typ', '.tf-diff__split'), 'class selector');
  assert.ok(has(doc, 'typ', ':hover'), 'pseudo-class');
  assert.ok(has(doc, 'prop', '[data-nav="1"]'), 'attribute selector');

  assert.ok(has(doc, 'prop', 'grid-template-columns'), 'declaration name');
  assert.ok(has(doc, 'prop', '--tf-gap'), 'custom property');
  assert.ok(has(doc, 'fn', 'rgba'));
  assert.ok(has(doc, 'num', '10px'));
  assert.ok(has(doc, 'num', '#0a0e22'));
  assert.ok(has(doc, 'kw', '!important'));
  assert.ok(has(doc, 'com', '/* trailing note */'));
});

test('css: a block comment may span lines', () => {
  const doc = tokenize('css', ['/* open', 'still comment', '*/ .x { color: red; }'].join('\n'));
  assert.equal(clsOf([doc[1]], 'still comment'), 'com');
  assert.ok(has([doc[2]], 'typ', '.x'));
});

// ---- shell -----------------------------------------------------------------

const SH_SAMPLE = `#!/usr/bin/env bash
set -euo pipefail

NAME="\${1:-tenta}"
COUNT=3

build() {
  local out="$PWD/target"
  echo "building $NAME into \${out}"
  if [ "$COUNT" -gt 0 ]; then
    printf '%s\\n' "$NAME"
  fi
}

cat <<'EOF'
literal $NOT_A_VAR here
EOF

cat <<-EOT
  indented body
EOT

build`;

test('shell: variables, expansions, keywords, heredocs and comments', () => {
  const doc = tokenize('shell', SH_SAMPLE);
  assertOrdered(doc, 'shell');

  assert.ok(has(doc, 'com', '#!/usr/bin/env bash'));
  assert.ok(has(doc, 'kw', 'set'));
  assert.ok(has(doc, 'kw', 'if'));
  assert.ok(has(doc, 'kw', 'then'));
  assert.ok(has(doc, 'kw', 'fi'));
  assert.ok(has(doc, 'kw', 'local'));

  assert.ok(has(doc, 'prop', '${1:-tenta}'), '${…} expansion');
  assert.ok(has(doc, 'prop', '${out}'));
  assert.ok(has(doc, 'prop', '$PWD'), '$VAR expansion');
  assert.ok(has(doc, 'prop', '$NAME'));
  assert.ok(has(doc, 'prop', '-euo'), 'option flag');

  assert.ok(has(doc, 'fn', 'build'), 'function definition');
  assert.ok(has(doc, 'fn', 'echo'));
  assert.ok(has(doc, 'prop', 'NAME'), 'assignment target');

  // Quoted heredoc: the body is literal, $NOT_A_VAR must NOT become a variable.
  assert.equal(clsOf([doc[15]], 'literal $NOT_A_VAR here'), 'str');
  assert.ok(!has([doc[15]], 'prop', '$NOT_A_VAR'));
  assert.ok(has(doc, 'kw', 'EOF'), 'the terminator line closes the heredoc');
  assert.equal(clsOf([doc[19]], '  indented body'), 'str');
  // `<<-` allows an indented terminator; the last statement must tokenize again.
  assert.ok(has([doc[22]], 'fn', 'build'));
});

test('shell: command substitution is scanned as code, not as text', () => {
  const doc = tokenize('shell', 'files=$(ls -1 "$DIR")');
  assert.ok(has(doc, 'prop', 'files'));
  assert.ok(has(doc, 'op', '$('));
  assert.ok(has(doc, 'fn', 'ls'));
  assert.ok(has(doc, 'prop', '-1'));
});

test('shell: a single-quoted string may span lines', () => {
  const doc = tokenize('shell', ["msg='one", "two'", 'echo 1'].join('\n'));
  assert.equal(clsOf([doc[1]], "two'"), 'str');
  assert.ok(has([doc[2]], 'fn', 'echo'));
});

// ---- TOML ------------------------------------------------------------------

const TOML_SAMPLE = `# workspace manifest
[package]
name = "tentaflow-core"
version = "0.1.0"
edition = 2024
released = 2026-08-14T09:30:00Z

[dependencies]
serde = { version = "1", features = ["derive"] }
tokio.workspace = true

[[bin]]
name = "tentaflow"

[profile.release]
lto = true
opt-level = 3
notes = """
multi line
"""`;

test('toml: table headers, keys, values, dates and comments', () => {
  const doc = tokenize('toml', TOML_SAMPLE);
  assertOrdered(doc, 'toml');

  assert.ok(has(doc, 'com', '# workspace manifest'));
  assert.ok(has(doc, 'kw', '[package]'), 'table header');
  assert.ok(has(doc, 'kw', '[[bin]]'), 'array-of-tables header');
  assert.ok(has(doc, 'kw', '[profile.release]'), 'dotted table header');

  assert.ok(has(doc, 'prop', 'name'));
  assert.ok(has(doc, 'prop', 'opt-level'), 'dashed key');
  assert.ok(has(doc, 'prop', 'tokio.workspace'), 'dotted key');
  assert.ok(has(doc, 'prop', 'version'), 'bare key inside an inline table');

  assert.ok(has(doc, 'str', '"tentaflow-core"'));
  assert.ok(has(doc, 'num', '2024'));
  assert.ok(has(doc, 'num', '2026-08-14T09:30:00Z'), 'RFC3339 datetime');
  assert.ok(has(doc, 'kw', 'true'));
});

test('toml: a multi-line basic string holds its state', () => {
  const doc = tokenize('toml', ['x = """', 'inside', '"""', 'y = 1'].join('\n'));
  assert.equal(clsOf([doc[1]], 'inside'), 'str');
  assert.ok(has([doc[3]], 'num', '1'));
  assert.ok(has([doc[3]], 'prop', 'y'));
});

// ---- regressions for the seven pre-existing languages -----------------------

test('python still tokenizes', () => {
  const doc = tokenize('python', 'class A:\n    @staticmethod\n    def f(x): return f"v={x}"  # note');
  assert.ok(has(doc, 'kw', 'class'));
  assert.ok(has(doc, 'kw', 'def'));
  assert.ok(has(doc, 'dec', '@staticmethod'));
  assert.ok(has(doc, 'fn', 'f'));
  assert.ok(has(doc, 'com', '# note'));
});

test('javascript and typescript still tokenize', () => {
  const js = tokenize('javascript', 'const a = `x${1}y`; // c\nfunction go() {}');
  assert.ok(has(js, 'kw', 'const'));
  assert.ok(has(js, 'fn', 'go'));
  assert.ok(has(js, 'com', '// c'));
  const ts = tokenize('typescript', 'interface X { a: number }');
  assert.ok(has(ts, 'kw', 'interface'));
  assert.ok(has(ts, 'kw', 'number'));
});

test('json still tokenizes', () => {
  const doc = tokenize('json', '{"a": 1, "b": [true, null]}');
  assert.ok(has(doc, 'prop', '"a"'));
  assert.ok(has(doc, 'num', '1'));
  assert.ok(has(doc, 'kw', 'true'));
});

test('yaml still tokenizes', () => {
  const doc = tokenize('yaml', 'key: value  # note\nlist:\n  - 1');
  assert.ok(has(doc, 'prop', 'key'));
  assert.ok(has(doc, 'com', '# note'));
  assert.ok(has(doc, 'num', '1'));
});

test('markdown still tokenizes', () => {
  const doc = tokenize('markdown', '# Title\n\n`code` and [link](http://x)');
  assert.ok(has(doc, 'kw', '# Title'));
  assert.ok(has(doc, 'str', '`code`'));
  assert.ok(has(doc, 'fn', '[link]'));
});

test('gherkin still tokenizes', () => {
  const doc = tokenize('gherkin', '@tag\nFeature: X\n  Given a thing\n  Then 3 results');
  assert.ok(has(doc, 'dec', '@tag'));
  assert.ok(has(doc, 'kw', 'Feature:'));
  assert.ok(has(doc, 'kw', 'Given'));
  assert.ok(has(doc, 'num', '3'));
});

test('an unknown language emits no tokens', () => {
  const doc = tokenize('plain', 'fn main() { }');
  assert.equal(all(doc).length, 0);
});
