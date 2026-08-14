// ===== File: scan.rs — repository walking and source file loading =====

use std::fs;
use std::path::{Path, PathBuf};

/// Comment syntax of a source file. Getting this wrong is not cosmetic: `//`
/// starts a comment in Rust but is integer division in Mojo, so a language
/// agnostic stripper silently truncates every tiled grid expression and makes
/// the shape rules read the wrong text.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Mojo,
    Data,
}

impl Lang {
    fn line_comment(self) -> Option<&'static str> {
        match self {
            Lang::Rust => Some("//"),
            Lang::Mojo => Some("#"),
            Lang::Data => None,
        }
    }
}

/// One source file loaded once and shared by every rule, so a full lint run
/// touches the disk a single time per file.
pub struct SourceFile {
    /// Repository-relative path with forward slashes; this is the baseline key.
    pub rel: String,
    pub lang: Lang,
    pub lines: Vec<String>,
}

impl SourceFile {
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
}

/// Which part of the tree a rule looks at. Scopes are explicit rather than
/// derived from the path inside each rule: a rule that silently changes its
/// own scope is how a gate stops catching what it was written for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Production Rust: crates/*/src/**. Tests and benches are excluded.
    RustSrc,
    /// Every Rust file in the workspace, tests and examples included.
    RustAll,
    /// Mojo kernel sources.
    Mojo,
    /// Generated kernel manifests.
    Manifest,
}

pub fn collect(root: &Path, scope: Scope) -> Vec<SourceFile> {
    let mut out = Vec::new();
    match scope {
        Scope::RustSrc => {
            walk(&root.join("crates"), root, &mut out, &|p| {
                is_ext(p, "rs") && in_src_dir(p)
            });
        }
        Scope::RustAll => {
            walk(&root.join("crates"), root, &mut out, &|p| is_ext(p, "rs"));
            walk(&root.join("xtask"), root, &mut out, &|p| is_ext(p, "rs"));
        }
        Scope::Mojo => {
            walk(&root.join("kernels"), root, &mut out, &|p| {
                is_ext(p, "mojo")
            });
        }
        Scope::Manifest => {
            walk(&root.join("kernels"), root, &mut out, &|p| {
                p.file_name().and_then(|n| n.to_str()) == Some("manifest.json")
            });
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

fn in_src_dir(p: &Path) -> bool {
    p.components()
        .any(|c| c.as_os_str() == "src" || c.as_os_str() == "build.rs")
}

fn lang_of(p: &Path) -> Lang {
    match p.extension().and_then(|e| e.to_str()) {
        Some("rs") => Lang::Rust,
        Some("mojo") => Lang::Mojo,
        _ => Lang::Data,
    }
}

fn is_ext(p: &Path, ext: &str) -> bool {
    p.extension().and_then(|e| e.to_str()) == Some(ext)
}

fn walk(dir: &Path, root: &Path, out: &mut Vec<SourceFile>, keep: &dyn Fn(&Path) -> bool) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // `target` and `build` hold generated artifacts; linting them reports
        // violations nobody can fix.
        if name == "target" || name == ".git" || name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk(&path, root, out, keep);
        } else if keep(&path) {
            if let Ok(text) = fs::read_to_string(&path) {
                out.push(SourceFile {
                    rel: rel_path(&path, root),
                    lang: lang_of(&path),
                    lines: text.lines().map(|l| l.to_string()).collect(),
                });
            }
        }
    }
}

pub fn rel_path(path: &Path, root: &Path) -> String {
    let rel: PathBuf = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    rel.to_string_lossy().replace('\\', "/")
}

/// Length of the block opened on `start`, counted in lines, by brace balance.
/// Returns 1 when the line opens no block. Used by the vendor-gate rule, where
/// what matters is how much code sits behind one condition.
pub fn block_len(lines: &[String], start: usize, lang: Lang) -> usize {
    let mut depth: i32 = 0;
    let mut seen_open = false;
    for (offset, line) in lines[start..].iter().enumerate() {
        let code = strip_comment(line, lang);
        for ch in code.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    seen_open = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        if seen_open && depth <= 0 {
            return offset + 1;
        }
        if offset > 4000 {
            break;
        }
    }
    1
}

/// Drops a trailing line comment so braces inside prose do not shift the depth.
/// String literals are left alone on purpose: a brace inside a literal is rare
/// in this repository and the alternative is a parser.
pub fn strip_comment(line: &str, lang: Lang) -> &str {
    match lang.line_comment().and_then(|marker| line.find(marker)) {
        Some(idx) => &line[..idx],
        None => line,
    }
}
