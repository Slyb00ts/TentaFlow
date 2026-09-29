// =============================================================================
// File: tentanas-helper/src/refusal.rs — a refusal the admin reads, as the
// helper words it (helper 0.17.4).
//
// The core's coded-refusal wire (`tentaflow-core` `tentanas/refusal.rs`):
//
//     refusal:<code>[?<key>=<value>[&<key>=<value>…]][ <sentence>]
//
// The helper uses it for the failures an admin can act on: the core forwards
// the helper's first stderr line unchanged when it is this wire (the job's
// error line), and a SnapRAID run's detail in this form is worded on the
// array's history. Everything else the helper says is one English sentence:
// an internal invariant, or a detail that only reaches a tooltip or a log.
//
// A parameter never carries an id: a disk is its kernel name (`disk`), an
// array its name (`array`), a pool its name (`pool`); counts are numbers.
// The sentence is English and may name more; the screen shows it only in a
// tooltip, ids scrubbed.
// =============================================================================

/// Every code this helper refuses with. The core holds each one to words in
/// all five locales (`dispatch::tentanas` scan tests), and this crate's scan
/// holds every `wire("…")` call to this list.
pub const CODES: &[&str] = &[
    // Elastic Array: journals and disks.
    "elastic_journal_unreadable",
    "elastic_disk_missing",
    "elastic_disk_changed",
    "elastic_disk_busy",
    "elastic_disk_has_signature",
    "elastic_disk_foreign_filesystem",
    "elastic_disk_mounted_elsewhere",
    "elastic_add_disk_signed",
    "elastic_restore_after_boot",
    "elastic_restart_required",
    // Elastic Array: what a SnapRAID run reports on the array's history.
    "elastic_snapraid_reported_errors",
    "elastic_fix_unrecoverable",
    "elastic_fix_nothing_marked",
    "elastic_fix_nothing_unchanged",
    "elastic_fix_recovered",
    "elastic_fix_partly_recovered",
    "elastic_sync_files_changed",
    "elastic_sync_interrupted",
    "elastic_scrub_interrupted",
    "elastic_fix_interrupted",
    // Codes the core already words, used by the helper for the same case.
    "elastic_name_unavailable",
    "elastic_disk_claimed",
    "elastic_operation_pending",
    "elastic_attention_add_disk",
    "elastic_attention_parity_fault",
    "disk_not_found",
    "disk_wipe_plan_changed",
    "disk_wipe_journal_unacknowledged",
    // ZFS commands guarded by the Elastic reservations.
    "zfs_name_reserved",
    "zfs_mountpoint_reserved",
    "zfs_disk_reserved",
    // Clearing a disk.
    "disk_wipe_holders",
    "disk_wipe_mounted",
    "disk_wipe_swap",
    "disk_wipe_zfs_member",
    "disk_wipe_md_member",
    "disk_wipe_elastic_serving",
    "disk_wipe_busy",
    "disk_wipe_readonly",
    "disk_wipe_release_failed",
    "disk_wipe_signatures_remain",
    "disk_wipe_journal_kept",
];

/// The wire form of a refusal: the code, its parameters (an empty value is
/// left out) and the English sentence, on one line.
pub fn wire(code: &'static str, params: &[(&'static str, &str)], sentence: impl AsRef<str>) -> String {
    debug_assert!(CODES.contains(&code), "{code} is not a helper refusal code");
    let mut out = format!("refusal:{code}");
    let mut first = true;
    for (key, value) in params {
        if value.is_empty() {
            continue;
        }
        out.push(if first { '?' } else { '&' });
        first = false;
        out.push_str(key);
        out.push('=');
        out.push_str(&encode(value));
    }
    let sentence = sentence.as_ref().split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = sentence.chars();
    if let Some(head) = chars.next() {
        out.push(' ');
        out.extend(head.to_uppercase());
        out.push_str(chars.as_str());
    }
    out
}

/// Whether `text` is a refusal wire, not a sentence: the code the core and
/// the screen read, then nothing, a `?` or a space.
pub fn is_wire(text: &str) -> bool {
    text.strip_prefix("refusal:").is_some_and(|rest| {
        let code_len = rest.bytes().take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_').count();
        code_len > 0 && matches!(rest.as_bytes().get(code_len), None | Some(b'?') | Some(b' '))
    })
}

/// The sentence of a refusal wire (what follows its code and parameters),
/// or `text` itself when it is not one: for a sentence that quotes another
/// refusal.
pub fn sentence(text: &str) -> &str {
    if is_wire(text) {
        text.split_once(' ').map_or("", |(_, sentence)| sentence)
    } else {
        text
    }
}

/// `error` with more said around its sentence: a refusal keeps its code and
/// parameters at the start of the line (the only place the core and the
/// screen read them) and gets `say(sentence)` as its sentence; any other text
/// is simply `say(text)`.
pub fn reword(error: &str, say: impl FnOnce(&str) -> String) -> String {
    if !is_wire(error) {
        return say(error);
    }
    let (head, sentence) = error.split_once(' ').unwrap_or((error, ""));
    let said = say(sentence);
    let mut chars = said.trim().chars();
    match chars.next() {
        // Capitalised like every sentence `wire` writes.
        Some(first) => format!("{head} {}{}", first.to_uppercase(), chars.as_str()),
        None => head.to_string(),
    }
}

/// Percent-encoding of every byte outside `A-Za-z0-9-_.~`
/// (`encodeURIComponent`-compatible, like the core's).
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_carries_the_code_the_parameters_and_one_sentence_line() {
        let text = wire("disk_wipe_mounted", &[("disk", "sdc"), ("array", "")], "the device is mounted\n at /mnt/x");
        assert_eq!(text, "refusal:disk_wipe_mounted?disk=sdc The device is mounted at /mnt/x");
        assert!(is_wire(&text));
        let spaced = wire("zfs_name_reserved", &[("pool", "my pool&x")], "");
        assert_eq!(spaced, "refusal:zfs_name_reserved?pool=my%20pool%26x");
        assert!(is_wire(&spaced));
        assert!(!is_wire("refusal: not a code"));
        assert!(!is_wire("refusal:Upper"));
        assert!(!is_wire("the helper said refusal:x"));
        assert_eq!(sentence(&text), "The device is mounted at /mnt/x");
        assert_eq!(sentence("refusal:disk_not_found"), "");
        assert_eq!(sentence("plain words"), "plain words");
        assert_eq!(
            reword(&text, |s| format!("sync after the disk addition: {s}")),
            "refusal:disk_wipe_mounted?disk=sdc Sync after the disk addition: The device is mounted at /mnt/x"
        );
        assert_eq!(reword("plain", |s| format!("context: {s}")), "context: plain");
    }

    #[test]
    fn every_code_is_a_wire_code_and_listed_once() {
        let mut seen = std::collections::BTreeSet::new();
        for code in CODES {
            assert!(code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'), "{code}");
            assert!(seen.insert(*code), "{code} is listed twice");
        }
    }
}
