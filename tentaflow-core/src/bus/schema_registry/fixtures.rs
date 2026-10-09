// =============================================================================
// File: bus/schema_registry/fixtures.rs — golden fixture loader for tests
// =============================================================================
// PLAN-F4-REST.md §B.10: per-format golden fixtures live under
// `tests/fixtures/bus_schema_registry/<format>/`. A document named
// `valid-*` must validate; `invalid-*` must be rejected with a violation
// containing the text of its `.expect` sibling.
// =============================================================================

use std::path::PathBuf;

pub(super) struct Case {
    pub name: String,
    pub payload: Vec<u8>,
    /// Text the violation must contain; `None` for `valid-*` documents.
    pub expect: Option<String>,
}

pub(super) fn dir(format: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/bus_schema_registry")
        .join(format)
}

pub(super) fn read(format: &str, name: &str) -> String {
    std::fs::read_to_string(dir(format).join(name))
        .unwrap_or_else(|e| panic!("fixture {format}/{name}: {e}"))
}

/// Every `valid-*` / `invalid-*` document with the given extension, in name order.
pub(super) fn cases(format: &str, extension: &str) -> Vec<Case> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir(format)).expect("fixture directory") {
        let path = entry.expect("fixture entry").path();
        let Some(file) = path.file_name().and_then(|f| f.to_str()) else {
            continue;
        };
        let Some(stem) = file.strip_suffix(&format!(".{extension}")) else {
            continue;
        };
        if !(stem.starts_with("valid-") || stem.starts_with("invalid-")) {
            continue;
        }
        let expect = stem.starts_with("invalid-").then(|| {
            std::fs::read_to_string(path.with_extension("expect"))
                .unwrap_or_else(|e| panic!("missing .expect for {file}: {e}"))
                .trim_end()
                .to_string()
        });
        out.push(Case {
            name: stem.to_string(),
            payload: std::fs::read(&path).expect("fixture bytes"),
            expect,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    assert!(
        !out.is_empty(),
        "no {extension} fixtures found for {format}"
    );
    out
}
