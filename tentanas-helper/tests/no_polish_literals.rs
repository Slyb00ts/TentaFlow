// =============================================================================
// File: tentanas-helper/tests/no_polish_literals.rs — the helper speaks
// English, and codes what an admin acts on (wave 16).
//
// The core puts the first line of the helper's stderr on a job's error line,
// and a SnapRAID run's detail on the array's history, so every sentence the
// helper writes can reach a screen. The ones an admin acts on are coded
// refusals (`src/refusal.rs`), worded by the screen in five locales; the rest
// is English. This scan reads EVERY production file of `src/` from disk — a
// file added later is guarded too — and holds it to that.
//
// The detector is the core's (`dispatch::tentanas`,
// `no_polish_literal_remains_in_the_production_code`): comments dropped, raw
// strings and char literals read, `#[cfg(test)]` items cut. The cut here also
// takes `#[cfg(all(test, …))]`, which the helper uses for its Linux-only test
// modules.
// =============================================================================

use std::path::Path;

/// The code of `source` with every comment dropped, and the string literals
/// it holds. A char literal becomes `'_'` and a raw string an ordinary one,
/// so a brace or a quote inside either never unbalances the matchers.
fn code_and_strings(source: &str) -> (String, Vec<String>) {
    let bytes = source.as_bytes();
    let mut code = String::with_capacity(source.len());
    let mut strings = Vec::new();
    let mut i = 0;
    let word_before = |i: usize| i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
    while i < bytes.len() {
        let rest = &source[i..];
        if rest.starts_with("//") {
            i += rest.find('\n').unwrap_or(rest.len());
            continue;
        }
        if rest.starts_with("/*") {
            i += rest.find("*/").map_or(rest.len(), |at| at + 2);
            continue;
        }
        let raw_at = if rest.starts_with("br") { 2 } else if rest.starts_with('r') { 1 } else { 0 };
        if raw_at > 0 && !word_before(i) {
            let hashes = rest[raw_at..].bytes().take_while(|b| *b == b'#').count();
            if rest[raw_at + hashes..].starts_with('"') {
                let open = raw_at + hashes + 1;
                let close = format!("\"{}", "#".repeat(hashes));
                let end = rest[open..].find(&close).map_or(rest.len(), |at| open + at);
                let content = &rest[open..end];
                strings.push(content.to_string());
                code.push('"');
                code.push_str(&content.replace('\\', "\\\\").replace('"', "\\\""));
                code.push('"');
                i += (end + close.len()).min(rest.len());
                continue;
            }
        }
        if bytes[i] == b'\'' {
            let len = if rest.starts_with("'\\") {
                rest.get(3..).and_then(|tail| tail.find('\'')).map(|at| at + 4)
            } else {
                let mut chars = rest[1..].chars();
                chars.next().and_then(|c| (chars.next() == Some('\'')).then(|| c.len_utf8() + 2))
            };
            if let Some(len) = len {
                code.push_str("'_'");
                i += len;
                continue;
            }
        }
        if bytes[i] == b'"' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'"' {
                if bytes[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            strings.push(source[i + 1..j.min(bytes.len())].to_string());
            code.push_str(&source[i..(j + 1).min(bytes.len())]);
            i = j + 1;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        code.push(ch);
        i += ch.len_utf8();
    }
    (code, strings)
}

/// The next test-only attribute in `code`: its start and the text right after
/// it. `#[cfg(test)]`, and any `#[cfg(…)]` that names `test` other than as
/// `not(test)` (`#[cfg(all(test, target_os = "linux"))]`).
fn next_test_attribute(code: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(at) = find_outside_strings(code, from, "#[cfg(") {
        let open = at + "#[cfg".len();
        let mut depth = 0usize;
        let mut close = None;
        for (offset, ch) in code[open..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + offset);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close?;
        let predicate = &code[open..=close];
        let end = close + code[close..].find(']').map_or(1, |at| at + 1);
        let names_test = predicate
            .match_indices("test")
            .any(|(t, _)| {
                let before = predicate[..t].chars().next_back();
                let after = predicate[t + 4..].chars().next();
                !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                    && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
                    && !predicate[..t].ends_with("not(")
            });
        if names_test {
            return Some((at, end));
        }
        from = end;
    }
    None
}

/// The first `needle` at or after `from` that is not inside a string
/// literal (`block.rs` compares lines against a `"#[cfg(test)]"` literal).
fn find_outside_strings(code: &str, from: usize, needle: &str) -> Option<usize> {
    let (mut in_string, mut escaped) = (false, false);
    for (i, ch) in code.char_indices() {
        if in_string {
            match (escaped, ch) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
        } else if i >= from && code[i..].starts_with(needle) {
            return Some(i);
        }
    }
    None
}

/// The code (comments already dropped) with every test-only item cut out:
/// a module or a function to its matching brace, a `const` or a `use` to its
/// semicolon, a field, a variant or an arm to its comma. Strings are skipped.
fn production(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut rest = code;
    while let Some((at, after)) = next_test_attribute(rest) {
        out.push_str(&rest[..at]);
        let item = &rest[after..];
        let mut head = item.trim_start();
        // Further attributes of the same item (`#[test]`, `#[allow(…)]`).
        while head.starts_with("#[") {
            head = head.find(']').map_or("", |at| head[at + 1..].trim_start());
        }
        if let Some(rest) = head.strip_prefix("pub(") {
            head = rest.split_once(')').map_or(rest, |(_, after)| after).trim_start();
        } else if let Some(rest) = head.strip_prefix("pub ") {
            head = rest.trim_start();
        }
        let is_item = [
            "fn ", "async ", "unsafe ", "mod ", "impl ", "impl<", "struct ", "enum ", "trait ", "type ", "const ",
            "static ", "use ", "macro_rules!",
        ]
        .iter()
        .any(|keyword| head.starts_with(keyword));
        let (mut depth, mut in_string, mut escaped, mut end) = (0usize, false, false, item.len());
        for (i, ch) in item.char_indices() {
            if in_string {
                match (escaped, ch) {
                    (true, _) => escaped = false,
                    (false, '\\') => escaped = true,
                    (false, '"') => in_string = false,
                    _ => {}
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                ';' if depth == 0 => {
                    end = i + 1;
                    break;
                }
                ',' if depth == 0 && !is_item => {
                    end = i + 1;
                    break;
                }
                '{' | '(' | '[' => depth += 1,
                '}' | ')' | ']' if depth == 0 => {
                    end = i;
                    break;
                }
                '}' | ')' | ']' => {
                    depth -= 1;
                    // An attribute's own brackets (`#[test]`) close at depth
                    // 0 without ending the item.
                    if depth == 0 && ch == '}' {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = &item[end..];
    }
    out.push_str(rest);
    out
}

/// Polish in a literal: a diacritic, or one of the words the helper's Polish
/// messages were written with (many carry no diacritic at all: "numer
/// slotu", "obcy plik", "nazwa journala").
fn polish_literal(text: &str) -> bool {
    const WORDS: &[&str] = &[
        // The core's list.
        "nie", "brak", "macierz", "macierzy", "dysk", "dysku", "dyskow", "zadanie", "zadania", "operacja",
        "operacji", "operacje", "jest", "bez", "sie", "juz", "oraz", "lub", "dla", "przez", "albo", "gdy", "czy",
        "jako", "tylko", "zostal", "zostala", "zostalo", "wynik", "wyniku", "intencja", "intencji", "niezgodna",
        "niezgodny", "utracono", "obca", "sprzeczne", "zapis", "przekracza", "wymaga", "dni", "odmowa", "inny",
        "movera",
        // The helper's own.
        "obcy", "obce", "obcej", "inna", "innej", "plik", "pliku", "katalog", "katalogu", "nazwa", "numer",
        "slotu", "odczyt", "odczytu", "nieznana", "nieznany", "nieczytelny", "brancha", "branchy", "unii",
        "unia", "kotwica", "kotwicy", "zmiana", "przed", "podczas", "poza", "zawiera", "rozmiar", "wycofanie",
        "publikacja", "publikacji", "proces", "procesu", "stan", "stanu", "krok", "metadane", "pula",
        "odmawia", "przyczyna", "uwagi", "diagnostyka", "specyfikacja", "rekord", "toku", "wpis", "zadanie",
        "urzadzenie", "sciezka", "kopia", "oryginal", "wymiany", "naprawa", "dodawanie", "dziennik",
        "niejednoznaczny", "niepoprawny", "niespodziewany", "werdykt", "niepusty", "publiczny", "globalnego",
        "nieczytelna",
    ];
    text.chars().any(|c| "ąćęłńóśźżĄĆĘŁŃÓŚŹŻ".contains(c))
        || text.split(|c: char| !c.is_alphanumeric()).any(|word| WORDS.contains(&word.to_lowercase().as_str()))
}

/// Every production source of `src/`, by file name.
fn sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources: Vec<(String, String)> = std::fs::read_dir(&root)
        .expect("the helper sources")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&path).expect("a readable source"))
        })
        .collect();
    sources.sort();
    sources
}

#[test]
fn no_polish_literal_remains_in_the_helper_s_production_code() {
    let sources = sources();
    assert!(sources.len() >= 9, "the scan found the modules ({})", sources.len());
    let mut found = Vec::new();
    let mut scanned = 0;
    for (file, source) in &sources {
        let code = production(&code_and_strings(source).0);
        let (_, strings) = code_and_strings(&code);
        scanned += strings.len();
        found.extend(strings.into_iter().filter(|s| polish_literal(s)).map(|s| format!("{file}: {s}")));
    }
    assert!(scanned > 2000, "the scan read the production literals ({scanned})");
    assert!(found.is_empty(), "Polish literals in the helper's production code: {found:#?}");
}

#[test]
fn the_detector_sees_the_old_sentences_and_the_cut_keeps_production_code() {
    for old in ["numer slotu", "obcy plik blokady Elastic", "nazwa journala", "brak transferu", "urządzenie jest swapem"] {
        assert!(polish_literal(old), "{old}");
    }
    for english in ["the device is busy", "no transfer", "unreadable /proc/swaps", "invalid Elastic UUID"] {
        assert!(!polish_literal(english), "{english}");
    }
    let cut = production(
        "fn a() { \"x\" }\n#[cfg(test)]\nmod t { fn b() { \"}\" } }\n#[cfg(all(test, target_os = \"linux\"))]\npub(crate) mod u { fn c() {} }\n#[cfg(not(test))]\nfn d() {}",
    );
    assert_eq!(cut, "fn a() { \"x\" }\n\n\n#[cfg(not(test))]\nfn d() {}");
    let fields = production("struct S { a: u8, #[cfg(test)] b: (u8, u8), c: u8 }\n#[cfg(test)]\n#[test]\nfn e() { \"t\" }\nfn f() {}");
    assert_eq!(fields, "struct S { a: u8,  c: u8 }\n\nfn f() {}");
    let quoted = production("fn g(l: &str) -> bool { l.starts_with(\"#[cfg(test)]\") }\nfn h() { \"kept\" }");
    assert_eq!(quoted, "fn g(l: &str) -> bool { l.starts_with(\"#[cfg(test)]\") }\nfn h() { \"kept\" }");
    let (_, strings) = code_and_strings("fn f() { let a = '{'; let c = r#\"say \"(\" now\"#; /* \"no\" */ g(\"x)\") }");
    assert_eq!(strings, vec!["say \"(\" now".to_string(), "x)".to_string()]);
}

/// Every code the helper builds a refusal with, as the literal after
/// `wire(` in the production code.
fn wire_codes() -> std::collections::BTreeMap<String, usize> {
    let mut codes = std::collections::BTreeMap::new();
    for (_, source) in sources() {
        let code = production(&code_and_strings(&source).0);
        for (at, _) in code.match_indices("wire(") {
            // `fn wire(` is the definition, not a use.
            if code[..at].ends_with("fn ") {
                continue;
            }
            let rest = code[at + "wire(".len()..].trim_start();
            if let Some(literal) = rest.strip_prefix('"') {
                let name = &literal[..literal.find('"').expect("a closed literal")];
                *codes.entry(name.to_string()).or_insert(0) += 1;
            }
        }
    }
    codes
}

/// The codes are a closed list the core words in five locales: a code the
/// helper sends without a place in `refusal::CODES` would reach the screen
/// with no words, and a listed code nothing sends is dead words.
#[test]
fn every_refusal_the_helper_sends_is_a_listed_code_and_every_listed_code_is_sent() {
    let used = wire_codes();
    assert!(used.len() >= 30, "the scan found the refusals ({used:?})");
    let listed: std::collections::BTreeSet<&str> = tentanas_helper::refusal::CODES.iter().copied().collect();
    let unlisted: Vec<&String> = used.keys().filter(|code| !listed.contains(code.as_str())).collect();
    assert!(unlisted.is_empty(), "refusal codes outside refusal::CODES: {unlisted:?}");
    let unused: Vec<&&str> = listed.iter().filter(|code| !used.contains_key(**code)).collect();
    assert!(unused.is_empty(), "listed refusal codes the helper never sends: {unused:?}");
}
