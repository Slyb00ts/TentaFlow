// ===== File: code_studio/redact.rs — the scrubber every audited string passes through =====
//
// A session's audit trail is written from material the user and the build
// produce: a command line typed in the terminal, the output of a compiler, a
// URL an agent fetched. Any of them can carry a credential — a token pasted
// into `git push https://<token>@host`, a `--password` flag, an `Authorization`
// header echoed by a verbose HTTP client. Without redaction the audit trail
// would itself become the leak it exists to detect (§13.4).
//
// Three rules shape the scrubber.
//
// **Redaction happens before the write, not before the read.** Everything here
// is called while an event, an artifact or an outbox row is being BUILT, so
// nothing unredacted ever reaches the disk. A filter applied at display time
// would leave the secret in the database forever.
//
// **The process environment is never logged at all.** Not redacted — absent.
// Environment blocks carry tickets, vault material and provider keys in bulk,
// and no pattern list is good enough to make that safe. There is deliberately
// no function in this module that accepts an environment map, so a future call
// site cannot log one "just this once".
//
// **One engine, expressed as SPANS.** Every rule reports the byte range of the
// material that must not survive; `redact_text` splices `[redacted]` over those
// ranges. The span form is what lets a consumer that cannot use a rewritten
// string — the terminal grid, which must mask cells in place without changing
// how many of them there are — apply the identical rule set instead of growing
// a second, weaker one.
//
// The cost is real and accepted (§7.8): a redacted line is occasionally a line
// that would have helped diagnose a failure. The remedy is to re-run the
// command, not to keep the raw text. The opposite cost is just as real and is
// bounded on purpose: a rule that eats an ordinary argument (`--author`,
// `--signoff`) rewrites history in the audit trail, so flag names are matched
// segment by segment rather than by substring.

use std::ops::Range;
use std::sync::OnceLock;

use regex::Regex;

/// What replaces every removed value. One fixed marker, so a reader can tell a
/// redaction from an empty value or a literal.
pub const REDACTED: &str = "[redacted]";

/// Minimum length of a bare string considered for the entropy rule. Shorter
/// values are far more often an identifier than a credential. Vendor tokens
/// start at 20 characters (`sk-` plus 16, an Azure DevOps PAT at 24), so the
/// bar sits there and not at a round 32 that would wave a 31-character
/// credential through.
const ENTROPY_MIN_LEN: usize = 20;

/// Redacts free text: command output, agent messages, error strings.
pub fn redact_text(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    let spans = secret_spans(input);
    if spans.is_empty() {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len() + spans.len() * REDACTED.len());
    let mut cursor = 0;
    for span in spans {
        out.push_str(&input[cursor..span.start]);
        out.push_str(REDACTED);
        cursor = span.end;
    }
    out.push_str(&input[cursor..]);
    out
}

/// Byte ranges of `input` that carry credential material, sorted and merged.
///
/// This is the whole rule set. `redact_text` is a thin splice over it, and the
/// terminal masks the same ranges cell by cell — a rule added here reaches both
/// without either growing its own copy.
pub fn secret_spans(input: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    collect_header_spans(input, &mut spans);
    collect_token_shape_spans(input, &mut spans);
    collect_url_spans(input, &mut spans);
    collect_flag_spans(input, &mut spans);
    collect_assignment_spans(input, &mut spans);
    collect_private_key_spans(input, &mut spans);
    merge(spans)
}

/// Redacts a command line ELEMENT BY ELEMENT. Joining argv into one string and
/// scrubbing that would lose the boundary that tells `--token` from its value,
/// and would silently change how the command reads in the audit trail.
pub fn redact_argv(argv: &[String]) -> Vec<String> {
    argv.iter()
        .enumerate()
        .map(|(index, arg)| {
            let previous = index.checked_sub(1).map(|i| argv[i].as_str());
            redact_argument(previous, arg)
        })
        .collect()
}

fn redact_argument(previous: Option<&str>, arg: &str) -> String {
    if let Some(previous) = previous {
        if is_secret_flag(previous) && !is_numeric(arg) && !arg.starts_with('-') {
            return REDACTED.to_string();
        }
        // A high-entropy blob directly after any flag is a credential often
        // enough that the audit value of keeping it does not justify the risk.
        if previous.starts_with('-') && looks_secret(arg) {
            return REDACTED.to_string();
        }
    }
    redact_text(arg)
}

/// Redacts the credential-bearing parts of one URL: the `user:pass@` userinfo
/// and every query parameter whose NAME says it carries a token, signature or
/// key — plus any value that looks like a secret whatever it is called.
pub fn redact_url(raw: &str) -> String {
    let spans = merge(url_spans(raw, 0));
    if spans.is_empty() {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len() + spans.len() * REDACTED.len());
    let mut cursor = 0;
    for span in spans {
        out.push_str(&raw[cursor..span.start]);
        out.push_str(REDACTED);
        cursor = span.end;
    }
    out.push_str(&raw[cursor..]);
    out
}

/// Where a PEM block starts and ends. The terminal needs the two markers
/// separately because it scrubs one screen line at a time and has to carry the
/// "inside a key" state across lines itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateKeyMarker {
    Begin,
    End,
}

/// Recognises the delimiter lines of a PEM private key. The body between them
/// is base64 that no other rule would look at, because it sits in no value
/// position at all.
pub fn private_key_marker(line: &str) -> Option<PrivateKeyMarker> {
    let upper = line.trim().to_ascii_uppercase();
    if !upper.contains("PRIVATE KEY") {
        return None;
    }
    if upper.contains("-----BEGIN") {
        Some(PrivateKeyMarker::Begin)
    } else if upper.contains("-----END") {
        Some(PrivateKeyMarker::End)
    } else {
        None
    }
}

// =============================================================================
// Rules
// =============================================================================

fn collect_header_spans(input: &str, spans: &mut Vec<Range<usize>>) {
    for caps in authorization_re().captures_iter(input) {
        let whole = caps.get(0).expect("group 0");
        let prefix = caps.get(1).expect("authorization prefix");
        spans.push(prefix.end()..whole.end());
    }
    for caps in bearer_re().captures_iter(input) {
        let value = caps.get(1).expect("bearer value");
        spans.push(value.range());
    }
}

fn collect_token_shape_spans(input: &str, spans: &mut Vec<Range<usize>>) {
    for m in token_shape_re().find_iter(input) {
        spans.push(m.range());
    }
}

fn collect_url_spans(input: &str, spans: &mut Vec<Range<usize>>) {
    for m in url_re().find_iter(input) {
        spans.extend(url_spans(m.as_str(), m.start()));
    }
}

/// Credential ranges inside ONE url, offset by `base` so the caller can use
/// them against the text the url was found in.
fn url_spans(raw: &str, base: usize) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let (scheme, authority_start) = match raw.find("://") {
        Some(index) => (&raw[..index], index + 3),
        None => ("", 0),
    };
    let rest = &raw[authority_start..];
    let authority_end = rest
        .find(['/', '?', '#'])
        .map(|index| authority_start + index)
        .unwrap_or(raw.len());
    let authority = &raw[authority_start..authority_end];

    if let Some(at) = authority.rfind('@') {
        let userinfo = &authority[..at];
        if let Some(span) = userinfo_span(scheme, userinfo) {
            spans.push(authority_start + span.start + base..authority_start + span.end + base);
        }
    }

    if let Some(query_start) = raw[authority_end..].find('?').map(|i| authority_end + i + 1) {
        let query_end = raw[query_start..]
            .find('#')
            .map(|i| query_start + i)
            .unwrap_or(raw.len());
        let mut cursor = query_start;
        for pair in raw[query_start..query_end].split('&') {
            let pair_start = cursor;
            cursor += pair.len() + 1; // the separator we split on
            let Some(eq) = pair.find('=') else {
                continue;
            };
            let (key, value) = (&pair[..eq], &pair[eq + 1..]);
            if value.is_empty() {
                continue;
            }
            if is_secret_query_key(key) || looks_secret(value) {
                let value_start = pair_start + eq + 1;
                spans.push(value_start + base..value_start + value.len() + base);
            }
        }
    }
    spans
}

/// Which part of `user[:password]` is credential material.
///
/// For `http`/`https` the whole userinfo is: git's own documentation tells a
/// user to write the PAT as `https://<token>@host/` (GitLab, Bitbucket, Azure
/// DevOps) or as `https://<token>:x-oauth-basic@host/` (GitHub, where the token
/// is the USER NAME). Keeping "the part that identifies who acted" would keep
/// the credential in half the deployments in existence.
///
/// For `ssh` and the scp-like form there is no password in the URL at all and
/// the user name is a login (`git@`), which the audit trail is better off
/// keeping — unless it is credential-shaped or a password half is present.
fn userinfo_span(scheme: &str, userinfo: &str) -> Option<Range<usize>> {
    if userinfo.is_empty() {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    if scheme == "http" || scheme == "https" || userinfo.contains(':') {
        return Some(0..userinfo.len());
    }
    let credential_shaped = looks_secret(userinfo) || token_shape_re().is_match(userinfo);
    credential_shaped.then(|| 0..userinfo.len())
}

fn collect_flag_spans(input: &str, spans: &mut Vec<Range<usize>>) {
    for caps in flag_eq_re().captures_iter(input) {
        push_flag_value(&caps, spans);
    }
    for caps in flag_space_re().captures_iter(input) {
        push_flag_value(&caps, spans);
    }
    for caps in short_password_flag_re().captures_iter(input) {
        let value = caps.get(2).expect("short password value");
        // `-p 5432` is a port on half the tools in existence; a numeric value
        // is never treated as a password.
        if !is_numeric(value.as_str()) {
            spans.push(value.range());
        }
    }
}

fn push_flag_value(caps: &regex::Captures<'_>, spans: &mut Vec<Range<usize>>) {
    let flag = caps.get(1).expect("flag name");
    let value = caps.get(2).expect("flag value");
    let text = value.as_str();
    // A flag whose value is another flag has no value at all.
    if text.starts_with('-') {
        return;
    }
    if is_secret_flag(flag.as_str()) {
        if !is_numeric(text) {
            spans.push(value.range());
        }
        return;
    }
    if looks_secret(text) {
        spans.push(value.range());
    }
}

fn collect_assignment_spans(input: &str, spans: &mut Vec<Range<usize>>) {
    for caps in assignment_re().captures_iter(input) {
        let name = caps.get(1).expect("assignment name");
        let value = caps.get(2).expect("assignment value");
        let text = value.as_str();
        if is_secret_name(name.as_str()) {
            if !is_numeric(text) {
                spans.push(value.range());
            }
            continue;
        }
        if looks_secret(text) {
            spans.push(value.range());
        }
    }
}

/// A PEM private key pasted into a terminal or printed by a `cat` is the one
/// credential shape that sits in NO value position: no flag, no `=`, no URL.
/// It is found by its delimiters instead, and an unterminated block (output cut
/// off mid-key) is redacted to the end of the input rather than let through.
fn collect_private_key_spans(input: &str, spans: &mut Vec<Range<usize>>) {
    let mut open: Option<usize> = None;
    let mut offset = 0;
    for line in input.split_inclusive('\n') {
        match private_key_marker(line) {
            Some(PrivateKeyMarker::Begin) if open.is_none() => open = Some(offset + line.len()),
            Some(PrivateKeyMarker::End) => {
                if let Some(start) = open.take() {
                    spans.push(start..offset);
                }
            }
            _ => {}
        }
        offset += line.len();
    }
    if let Some(start) = open {
        spans.push(start..input.len());
    }
}

// =============================================================================
// Vocabulary
// =============================================================================

/// Names that carry credential material in a query string, an assignment or a
/// flag.
const SECRET_WORDS: &[&str] = &[
    "token",
    "tokens",
    "pat",
    "password",
    "passwd",
    "pass",
    "secret",
    "apikey",
    "accesskey",
    "credential",
    "credentials",
    "auth",
    "signature",
    "sig",
    "key",
];

/// Splits a flag or variable name into the words a human wrote it from, so
/// `--access-token`, `--access_token` and `ACCESS.TOKEN` all resolve to
/// `["access", "token"]`.
fn segments(name: &str) -> impl Iterator<Item = &str> {
    name.split(|c: char| c == '-' || c == '_' || c == '.' || c == ' ')
        .filter(|part| !part.is_empty())
}

/// Segment-exact matching. `--author` and `--signoff` contain `auth` and `sig`
/// as substrings; a substring rule therefore erased the commit author and the
/// sign-off from the audit trail — a redactor rewriting who did what is worse
/// than the leak it was aiming at.
fn is_secret_name(name: &str) -> bool {
    let lower = name.trim().trim_start_matches('-').to_ascii_lowercase();
    // Bound rather than returned directly: the iterator borrows `lower`, and a
    // tail expression is dropped after the block's locals.
    let matched = segments(&lower).any(|part| SECRET_WORDS.contains(&part));
    matched
}

/// Query keys are matched more loosely than flags: a signed URL writes them as
/// `X-Amz-Signature`, `sig`, `accesstoken` or `hmac` with no separator to split
/// on, and over-redacting one query parameter costs nothing an audit reader
/// needs.
fn is_secret_query_key(key: &str) -> bool {
    if is_secret_name(key) {
        return true;
    }
    let lower = key.trim().to_ascii_lowercase();
    SECRET_WORDS.iter().any(|word| lower.contains(word))
}

fn is_secret_flag(flag: &str) -> bool {
    if !flag.starts_with('-') {
        return false;
    }
    let lower = flag.trim_start_matches('-').to_ascii_lowercase();
    // `-p` is the classic short password flag (mysql, psql, docker login).
    lower == "p" || is_secret_name(&lower)
}

fn is_numeric(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}

/// Whether a bare string is credential-shaped: long enough, drawn from a
/// base64/hex/percent-encoded alphabet, and not obviously a path or a sentence.
/// It is applied ONLY in positions where a secret is plausible — after `=`,
/// after a flag, in a URL — never to arbitrary prose, which would shred normal
/// build output.
fn looks_secret(value: &str) -> bool {
    if value.len() < ENTROPY_MIN_LEN {
        return false;
    }
    // `%` and `~` keep a percent-encoded credential in scope: a token that
    // survived one round of URL encoding (`ghp%5F…`, a signature carried as
    // `%2F`) is still the credential.
    if !value.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-' | b'.' | b'%' | b'~')
    }) {
        return false;
    }
    // A path or a dotted version string is not a secret, however long it is.
    if value.bytes().filter(|b| *b == b'/').count() > 1 {
        return false;
    }
    if value.bytes().filter(|b| *b == b'.').count() > 2 {
        return false;
    }
    let decoded = percent_decode(value);
    let candidate = decoded.as_deref().unwrap_or(value);
    if candidate.bytes().all(|b| b.is_ascii_hexdigit()) {
        return true;
    }
    let has_digit = candidate.bytes().any(|b| b.is_ascii_digit());
    let has_alpha = candidate.bytes().any(|b| b.is_ascii_alphabetic());
    has_digit && has_alpha && shannon_entropy(candidate) >= 3.0
}

/// Percent-decoding for the entropy test only. `Some` when the value really was
/// encoded, so the callers above can score the credential rather than its
/// escaping.
fn percent_decode(value: &str) -> Option<String> {
    if !value.contains('%') {
        return None;
    }
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &value[index + 1..index + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte as char);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index] as char);
        index += 1;
    }
    Some(out)
}

fn shannon_entropy(value: &str) -> f64 {
    let mut counts = [0usize; 256];
    for byte in value.bytes() {
        counts[byte as usize] += 1;
    }
    let len = value.len() as f64;
    counts
        .iter()
        .filter(|count| **count > 0)
        .map(|count| {
            let p = *count as f64 / len;
            -p * p.log2()
        })
        .sum()
}

fn merge(mut spans: Vec<Range<usize>>) -> Vec<Range<usize>> {
    if spans.len() < 2 {
        return spans;
    }
    spans.sort_by_key(|span| (span.start, span.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => merged.push(span),
        }
    }
    merged
}

// =============================================================================
// Patterns
// =============================================================================

fn authorization_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)(authorization\s*[:=]\s*)(?:bearer\s+|basic\s+|token\s+)?[^\s"',;]+"#)
            .expect("authorization regex")
    })
}

fn bearer_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\bbearer\s+([A-Za-z0-9._~+/=-]{8,})").expect("bearer regex")
    })
}

/// Vendor token shapes. Each one is a credential wherever it appears, so these
/// are removed without looking at the surrounding syntax.
fn token_shape_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            // The word boundary is load-bearing: without it `sk-` matches
            // inside `task-something-long` and shreds ordinary output.
            r"(?x)
              \b gh[pousr]_[A-Za-z0-9]{16,}
            | \b github_pat_[A-Za-z0-9_]{20,}
            | \b glpat-[A-Za-z0-9_-]{16,}
            | \b xox[baprs]-[A-Za-z0-9-]{8,}
            | \b sk-[A-Za-z0-9_-]{16,}
            | \b AKIA[0-9A-Z]{16}
            | \b eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}(?:\.[A-Za-z0-9_-]+)?
            ",
        )
        .expect("token shape regex")
    })
}

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"[a-zA-Z][a-zA-Z0-9+.-]*://[^\s"'<>\\]+"#).expect("url regex"))
}

fn flag_eq_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // The flag has to START a word: without the boundary, the `-cli` inside
        // `vendor-cli --token s3cr3t` matched as a flag whose "value" was the
        // real flag, and the credential after it was never examined.
        Regex::new(r#"(?:^|[\s"',;=])(--?[A-Za-z][A-Za-z0-9._-]*)[ \t]*=[ \t]*"?([^\s"']+)"#)
            .expect("flag assignment regex")
    })
}

fn flag_space_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?:^|[\s"',;])(--?[A-Za-z][A-Za-z0-9._-]*)[ \t]+"?([^\s"']+)"#)
            .expect("flag value regex")
    })
}

fn short_password_flag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)(^|\s)-p(\S{4,})").expect("short password flag regex"))
}

fn assignment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)\b([A-Za-z_][A-Za-z0-9_.-]*)[ \t]*=[ \t]*"?([^\s"';,]+)"#)
            .expect("assignment regex")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GITHUB_TOKEN: &str = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
    /// Split so the source carries no literal a secret scanner would flag: the
    /// fixture must LOOK like a Slack bot token for the rule to fire on it, but
    /// a repo-wide scan (GitHub push protection) rejects the assembled form.
    const SLACK_TOKEN: &str = concat!("xoxb", "-4839203948-ABCDEFGHIJKLMNOPQRST");
    /// A credential with no vendor prefix at all: the only rule that can catch
    /// it is the one under test in each case.
    const OPAQUE: &str = "Ab3xK9mQ7pL2vR5tW8yZ1nC4jH6sD0fG";

    #[test]
    fn adversarial_a_bare_token_in_the_userinfo_survives_redact_url() {
        // `redact_userinfo` only redacts the half AFTER a colon, and returns the
        // authority untouched when there is no colon at all. Both of the two
        // shapes a git remote actually carries a PAT in are therefore preserved:
        //
        //   https://<token>@host/...              (GitLab, Azure DevOps, Bitbucket)
        //   https://<token>:x-oauth-basic@host/   (GitHub's documented form —
        //                                          the token is the USERNAME)
        //
        // `redact_url` is called directly, never through `redact_text`, at
        // events.rs (`Egress.url`, `GitOp.remote`) and operations.rs (journal
        // input). `GitOperation::Push` is security-relevant, so the value is
        // copied into the org-wide `audit_log` by audit_outbox.
        //
        // Deliberately NOT a `ghp_`-shaped value: the existing test
        // `a_token_typed_into_the_terminal_never_survives_argv` passes only
        // because `token_shape_re` catches the prefix in a separate pass, so it
        // would stay green with `redact_userinfo` deleted entirely.
        const OPAQUE: &str = "Ab3xK9mQ7pL2vR5tW8yZ1nC4jH6sD0fG";

        let bare = redact_url(&format!("https://{OPAQUE}@gitlab.example/org/repo.git"));
        assert!(
            !bare.contains(OPAQUE),
            "a colon-less userinfo credential survived redaction: {bare}"
        );

        let as_username = redact_url(&format!(
            "https://{OPAQUE}:x-oauth-basic@github.com/org/repo.git"
        ));
        assert!(
            !as_username.contains(OPAQUE),
            "a credential carried as the user name survived redaction: {as_username}"
        );
    }

    #[test]
    fn a_url_credential_is_removed_whatever_the_shape_and_the_target_survives() {
        for raw in [
            format!("https://{OPAQUE}@gitlab.example/org/repo.git"),
            format!("https://{OPAQUE}:x-oauth-basic@github.com/org/repo.git"),
            format!("https://user:{OPAQUE}@github.com/org/repo.git"),
            format!("https://{OPAQUE}@github.com:8443/org/repo.git"),
        ] {
            let out = redact_url(&raw);
            assert!(!out.contains(OPAQUE), "credential survived in {out}");
            assert!(out.contains(REDACTED), "no marker was left behind: {out}");
            assert!(
                out.contains("/org/repo.git"),
                "the url lost its identity: {out}"
            );
        }

        // An ssh login name is not a credential and the trail is better off
        // keeping it — that is who acted.
        let ssh = redact_url("ssh://git@github.com/org/repo.git");
        assert_eq!(ssh, "ssh://git@github.com/org/repo.git");
        assert_eq!(
            redact_url("git@github.com:org/repo.git"),
            "git@github.com:org/repo.git"
        );
        // …unless the "login name" is the credential.
        let scp = redact_url(&format!("{OPAQUE}@github.com:org/repo.git"));
        assert!(!scp.contains(OPAQUE), "{scp}");
    }

    #[test]
    fn a_credential_in_argv_never_survives_whatever_named_it() {
        let argv: Vec<String> = vec![
            "git".into(),
            "push".into(),
            format!("https://{OPAQUE}@github.com/org/repo.git"),
            "--token".into(),
            OPAQUE.into(),
            "--password".into(),
            "s3cr3t-typed-by-a-human".into(),
            "main".into(),
        ];
        let redacted = redact_argv(&argv);

        assert_eq!(redacted[0], "git");
        assert_eq!(redacted[1], "push");
        assert!(
            !redacted[2].contains(OPAQUE),
            "the token survived in the remote url: {}",
            redacted[2]
        );
        assert!(
            redacted[2].contains("github.com/org/repo.git"),
            "the url lost its identity"
        );
        assert_eq!(redacted[4], REDACTED, "the value after --token survived");
        assert_eq!(
            redacted[6], REDACTED,
            "a short value after --password survived"
        );
        assert_eq!(redacted[7], "main", "an ordinary argument was destroyed");
    }

    #[test]
    fn a_secret_flag_takes_its_value_with_a_space_as_well_as_an_equals_sign() {
        for line in [
            "vendor-cli --token s3cr3t --verbose",
            "vendor-cli --token=s3cr3t --verbose",
            "vendor-cli --api-key s3cr3t --verbose",
            "vendor-cli --pat s3cr3t --verbose",
        ] {
            let out = redact_text(line);
            assert!(!out.contains("s3cr3t"), "{out}");
            assert!(out.contains("--verbose"), "the next flag was eaten: {out}");
        }
    }

    #[test]
    fn a_credential_one_character_below_the_old_floor_is_still_a_credential() {
        // 31 characters: under the previous 32-character entropy floor.
        let value = "Ab3xK9mQ7pL2vR5tW8yZ1nC4jH6sD0f";
        assert_eq!(value.len(), 31);
        let out = redact_text(&format!("SESSION={value}"));
        assert!(!out.contains(value), "{out}");
    }

    #[test]
    fn a_url_encoded_credential_is_recognised_through_its_escaping() {
        let out = redact_text("callback=https://example.com/x?state=ok&sig=aGVsbG8%2Fd29ybGQ%2BMTIzNDU2Nzg5");
        assert!(!out.contains("aGVsbG8%2Fd29ybGQ"), "{out}");
    }

    #[test]
    fn a_private_key_block_never_survives_even_unterminated() {
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\n\
                   b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAABlwAAAAdz\n\
                   c2gtcnNhAAAAAwEAAQAAAYEAqvVvT2b9Yc0N1vTbGvCq7XlLzL5nTfP0Yy1kQmZm\n\
                   -----END OPENSSH PRIVATE KEY-----\n";
        let out = redact_text(&format!("$ cat id_ed25519\n{key}$ echo done\n"));
        assert!(!out.contains("b3BlbnNzaC1rZXktdjEA"), "{out}");
        assert!(!out.contains("c2gtcnNhAAAAAwEAAQ"), "{out}");
        assert!(
            out.contains("-----BEGIN OPENSSH PRIVATE KEY-----"),
            "the marker is not the secret and reading the trail needs it: {out}"
        );
        assert!(out.contains("$ echo done"), "output after the key was lost");

        // Output cut off mid-key still has no body left in it.
        let truncated = redact_text(
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEAxLbTgUq0Jb3vQnQ0\n",
        );
        assert!(!truncated.contains("MIIEpAIBAAKCAQEA"), "{truncated}");
    }

    #[test]
    fn inline_flag_values_and_short_password_flags_are_redacted() {
        let line = "mysql -h db -pHunter2Hunter2 --password=letmein --port 5432 -p 3306";
        let out = redact_text(line);
        assert!(!out.contains("Hunter2Hunter2"), "{out}");
        assert!(!out.contains("letmein"), "{out}");
        assert!(
            out.contains("--port 5432"),
            "a port was mistaken for a secret: {out}"
        );
        assert!(
            out.contains("-p 3306"),
            "a numeric -p value was redacted: {out}"
        );
    }

    #[test]
    fn command_output_loses_headers_vendor_tokens_and_high_entropy_values() {
        let output = format!(
            "> GET /v1/models\n\
             > Authorization: Bearer {GITHUB_TOKEN}\n\
             AWS_ACCESS_KEY_ID=AKIA1234567890ABCDEF\n\
             SLACK={SLACK_TOKEN}\n\
             JWT: eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk\n\
             SESSION_KEY=aGVsbG8xMjM0NTY3ODkwYWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXo=\n\
             Compiling tentaflow-core v0.1.0 (/mnt/d/repos/TentaFlow/tentaflow-core)\n"
        );
        let out = redact_text(&output);

        assert!(!out.contains(GITHUB_TOKEN), "{out}");
        assert!(!out.contains("AKIA1234567890ABCDEF"), "{out}");
        assert!(
            !out.contains(SLACK_TOKEN),
            "{out}"
        );
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9."), "{out}");
        assert!(!out.contains("aGVsbG8xMjM0NTY3ODkwYWJjZGVm"), "{out}");
        // Ordinary build output must remain readable, otherwise the audit
        // trail is useless and people will turn it off.
        assert!(
            out.contains("Compiling tentaflow-core v0.1.0"),
            "normal build output was mangled: {out}"
        );
        assert!(out.contains("> GET /v1/models"), "{out}");
    }

    #[test]
    fn a_signed_url_keeps_its_target_and_loses_its_credentials() {
        let url = "https://storage.example.com/bucket/object?token=abc123def456&expires=1700000000&sig=ZmFrZXNpZ25hdHVyZQ";
        let out = redact_url(url);
        assert!(
            out.starts_with("https://storage.example.com/bucket/object?"),
            "{out}"
        );
        assert!(!out.contains("abc123def456"), "{out}");
        assert!(!out.contains("ZmFrZXNpZ25hdHVyZQ"), "{out}");
        assert!(
            out.contains("expires=1700000000"),
            "a harmless parameter was lost: {out}"
        );

        // The same URL inside a line of output is redacted too.
        let inline = redact_text(&format!("fetching {url} ..."));
        assert!(!inline.contains("abc123def456"), "{inline}");
        assert!(
            inline.contains("fetching https://storage.example.com/bucket/object?"),
            "{inline}"
        );
    }

    #[test]
    fn the_redactor_never_rewrites_the_history_it_exists_to_record() {
        // Over-redaction is a defect of the same class as under-redaction: a
        // rule matching `auth` inside `--author` erased the commit author from
        // the audit trail, and `sig` inside `--signoff` erased the sign-off.
        let argv: Vec<String> = vec![
            "git".into(),
            "commit".into(),
            "--author".into(),
            "Piotr Jarocki <piotr.jarocki@euvic.pl>".into(),
            "--signoff".into(),
            "true".into(),
            "--message".into(),
            "fix the parser".into(),
        ];
        let redacted = redact_argv(&argv);
        assert_eq!(
            redacted[3], "Piotr Jarocki <piotr.jarocki@euvic.pl>",
            "the commit author was rewritten by the redactor"
        );
        assert_eq!(redacted[5], "true", "the sign-off was rewritten");
        assert_eq!(redacted[7], "fix the parser");

        let line = redact_text("git commit --author=\"Piotr Jarocki\" --signoff --amend");
        assert!(line.contains("Piotr Jarocki"), "{line}");

        for benign in [
            "cargo test --package tentaflow-core --lib code_studio",
            "error[E0433]: failed to resolve: use of undeclared crate `serde_yaml`",
            "path=/mnt/d/repos/TentaFlow/tentaflow-core/src/code_studio/events.rs",
            "warning: unused variable: `secret_ref`",
        ] {
            assert_eq!(redact_text(benign), benign, "a benign line was altered");
        }

        // A 40-char hex OID in a value position IS entropy-shaped; the journal
        // keeps OIDs in typed columns, not in scrubbed text.
        let commit = redact_text("commit=9f2a1c4b0e5d4a779c318a2b6d4e1f0012345678");
        assert!(commit.contains(REDACTED), "{commit}");
    }

    #[test]
    fn redaction_is_stable_under_a_second_pass() {
        let once = redact_text(&format!(
            "--token={GITHUB_TOKEN} https://u:p@h/x?key=abcdefghijklmnopqrstuvwxyz012345"
        ));
        let twice = redact_text(&once);
        assert_eq!(once, twice, "redaction is not idempotent");
        assert!(!once.contains(GITHUB_TOKEN));
    }

    #[test]
    fn spans_and_text_are_the_same_rule_set() {
        // The terminal masks cells over `secret_spans`; `redact_text` splices
        // the marker over the same ranges. If the two ever disagree, one of the
        // sinks is unprotected.
        let line = format!("git push https://{OPAQUE}@github.com/org/repo.git --token {GITHUB_TOKEN}");
        let spans = secret_spans(&line);
        assert!(!spans.is_empty());
        for span in &spans {
            let covered = &line[span.clone()];
            assert!(
                !covered.is_empty() && line.is_char_boundary(span.start) && line.is_char_boundary(span.end),
                "span {span:?} is not a usable slice"
            );
        }
        let masked: String = {
            let mut out = String::new();
            let mut cursor = 0;
            for span in &spans {
                out.push_str(&line[cursor..span.start]);
                out.push_str(&"*".repeat(span.end - span.start));
                cursor = span.end;
            }
            out.push_str(&line[cursor..]);
            out
        };
        assert!(!masked.contains(OPAQUE), "{masked}");
        assert!(!masked.contains(GITHUB_TOKEN), "{masked}");
        assert_eq!(masked.len(), line.len(), "masking changed the width");
    }
}
