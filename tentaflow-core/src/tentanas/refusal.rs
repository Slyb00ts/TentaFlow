// =============================================================================
// File: tentanas/refusal.rs — a refusal the admin reads, as a code with
//       parameters (wave 13).
//
// A refusal crosses the wire as the WHOLE `ProtocolError.message` (and, when a
// job is refused, as the job's `error`):
//
//     refusal:<code>[?<key>=<value>[&<key>=<value>…]][ <sentence>]
//
// - `<code>` is `[a-z0-9_]+` and names the words the screen shows
//   (`tentanas.refusal.<code>` in the five locales);
// - the parameters are percent-encoded (every byte outside
//   `A-Za-z0-9-_.~`), so the token before the first space never contains a
//   space, a `&` or a `=` of a value;
// - `<sentence>` is the node's own English sentence, after ONE space. A screen
//   of this build shows it only in a tooltip or a detail (ids scrubbed); a
//   screen from before wave 13 shows the whole message, so it still reads the
//   sentence.
//
// A parameter never carries an id. A disk is named by its kernel name
// (`disk`), else by its place in the array (`data` / `parity` = the 1-based
// number, `cache` = "1"), else by its model (`model`); an array by its name
// (`array`). The screen turns whichever of the disk parameters it gets into
// one `{disk}` phrase.
//
// The older form `refusal:<code>` (no parameters, no sentence) is the same
// format with both parts empty, so every code sent before wave 13 still reads.
// =============================================================================

use tentaflow_protocol::{ProtocolError, ProtocolErrorCode};

/// A refusal: the code the screen words, its parameters, the node's English
/// sentence, and the protocol code the request is answered with.
///
/// It is an `std::error::Error`, so it travels inside an `anyhow::Error` from
/// the store or the Elastic layer up to the dispatcher, which answers with it
/// as it is (`Refusal::find`) instead of an internal error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub params: Vec<(&'static str, String)>,
    pub text: String,
    pub status: ProtocolErrorCode,
}

impl Refusal {
    fn with(status: ProtocolErrorCode, code: &'static str, text: impl Into<String>) -> Self {
        debug_assert!(code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
        Self { code, params: Vec::new(), text: text.into(), status }
    }

    /// The request cannot run in the array's (or the node's) present state.
    pub fn not_available(code: &'static str, text: impl Into<String>) -> Self {
        Self::with(ProtocolErrorCode::NotAvailable, code, text)
    }

    /// The request itself is not one this node takes.
    pub fn bad_request(code: &'static str, text: impl Into<String>) -> Self {
        Self::with(ProtocolErrorCode::BadRequest, code, text)
    }

    /// What the request names is not here.
    pub fn not_found(code: &'static str, text: impl Into<String>) -> Self {
        Self::with(ProtocolErrorCode::NotFound, code, text)
    }

    /// Something else already holds what the request asks for.
    pub fn conflict(code: &'static str, text: impl Into<String>) -> Self {
        Self::with(ProtocolErrorCode::Conflict, code, text)
    }

    /// One parameter. An empty value is not sent: the screen reads a missing
    /// parameter and an empty one the same way, and the wire stays shorter.
    pub fn param(mut self, key: &'static str, value: impl ToString) -> Self {
        let value = value.to_string();
        if !value.is_empty() {
            self.params.push((key, value));
        }
        self
    }

    /// The disk parameters (see the file header): the kernel name when there
    /// is one, else the place in the array, else the model.
    pub fn disk(self, words: DiskWords) -> Self {
        match words {
            DiskWords::Kernel(name) => self.param("disk", name),
            DiskWords::Data(n) => self.param("data", n),
            DiskWords::Parity(n) => self.param("parity", n),
            DiskWords::Cache => self.param("cache", 1),
            DiskWords::Model(model) => self.param("model", model),
            DiskWords::Unnamed => self,
        }
    }

    /// The wire form (the file header).
    pub fn wire(&self) -> String {
        wire_form(self.code, self.params.iter().map(|(k, v)| (*k, v.as_str())), &self.text)
    }

    /// The refusal an error carries anywhere in its chain, if any.
    pub fn find(error: &anyhow::Error) -> Option<&Refusal> {
        error.chain().find_map(|cause| cause.downcast_ref::<Refusal>())
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.wire())
    }
}

impl std::error::Error for Refusal {}

impl From<Refusal> for ProtocolError {
    fn from(refusal: Refusal) -> Self {
        ProtocolError::new(refusal.status, refusal.wire())
    }
}

impl From<&Refusal> for ProtocolError {
    fn from(refusal: &Refusal) -> Self {
        ProtocolError::new(refusal.status, refusal.wire())
    }
}

/// The wire form of a code, its parameters and a sentence — for a
/// `Refusal`, and for one stored as data (a schedule's outcome) and sent
/// again later.
pub fn wire_form<'a>(code: &str, params: impl IntoIterator<Item = (&'a str, &'a str)>, text: &str) -> String {
    let mut out = format!("refusal:{code}");
    for (index, (key, value)) in params.into_iter().enumerate() {
        out.push(if index == 0 { '?' } else { '&' });
        out.push_str(key);
        out.push('=');
        out.push_str(&encode(value));
    }
    // One line, starting with a capital: the sentence is read as it is by a
    // screen that has no words for the code.
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = text.chars();
    if let Some(first) = chars.next() {
        out.push(' ');
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
    }
    out
}

/// `anyhow::ensure!` for a refusal the admin reads: `Ok` when `ok`, else the
/// refusal as the error (built only then).
pub fn require(ok: bool, refusal: impl FnOnce() -> Refusal) -> anyhow::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(refusal().into())
    }
}

/// How a refusal names a disk, best first. Never an id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskWords {
    /// `sdb`, `nvme0n1`: the name the Disks tab shows.
    Kernel(String),
    /// The 1-based number of a data disk of the array.
    Data(usize),
    /// The 1-based number of a parity disk of the array.
    Parity(usize),
    /// The array's cache disk.
    Cache,
    /// The model of a disk this node no longer sees.
    Model(String),
    /// Nothing that is not an id is known.
    Unnamed,
}

impl DiskWords {
    /// A member of an array by its slot key (`d2`, `parity1`, `c1`) and the
    /// kernel name the inventory has for it ('' when none): the kernel name
    /// first, else the place the slot stands for. A slot of no known shape
    /// is not a name either.
    pub fn member(slot: &str, kernel_name: &str) -> Self {
        if !kernel_name.is_empty() {
            return Self::Kernel(kernel_name.to_string());
        }
        if let Some(n) = slot.strip_prefix("parity").and_then(|n| n.parse().ok()) {
            return Self::Parity(n);
        }
        if let Some(n) = slot.strip_prefix('d').and_then(|n| n.parse().ok()) {
            return Self::Data(n);
        }
        if slot.starts_with('c') {
            return Self::Cache;
        }
        Self::Unnamed
    }

    /// The English words, for the node's sentence.
    pub fn english(&self) -> String {
        match self {
            Self::Kernel(name) => format!("disk {name}"),
            Self::Data(n) => format!("data disk no. {n}"),
            Self::Parity(n) => format!("parity disk no. {n}"),
            Self::Cache => "the cache disk".to_string(),
            Self::Model(model) => format!("disk {model}"),
            Self::Unnamed => "the disk".to_string(),
        }
    }
}

/// Percent-encodes every byte outside the URL-unreserved set, the way
/// `encodeURIComponent` does for these bytes, so `decodeURIComponent` on the
/// screen gives the value back.
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

/// The parts of a wire refusal — `(code, params, sentence)` — for tests and
/// for any Rust reader of a stored job error.
pub fn parse(message: &str) -> Option<(String, Vec<(String, String)>, String)> {
    let rest = message.strip_prefix("refusal:")?;
    let (token, text) = rest.split_once(' ').unwrap_or((rest, ""));
    let (code, query) = token.split_once('?').unwrap_or((token, ""));
    if code.is_empty() || !code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
        return None;
    }
    let mut params = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.push((key.to_string(), decode(value)?));
    }
    Some((code.to_string(), params, text.to_string()))
}

fn decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_without_parameters_or_sentence_is_the_old_form() {
        let bare = Refusal::not_available("elastic_add_joined", "");
        assert_eq!(bare.wire(), "refusal:elastic_add_joined");
        assert_eq!(bare.to_string(), bare.wire());
    }

    #[test]
    fn parameters_are_percent_encoded_and_the_sentence_follows_one_space() {
        let refusal = Refusal::conflict("elastic_destroy_shared", "The array serves shares:\n a & b")
            .param("array", "media")
            .param("shares", "kadry, a&b=c ż")
            .param("empty", "");
        let wire = refusal.wire();
        assert_eq!(
            wire,
            "refusal:elastic_destroy_shared?array=media&shares=kadry%2C%20a%26b%3Dc%20%C5%BC The array serves shares: a & b"
        );
        let (code, params, text) = parse(&wire).unwrap();
        assert_eq!(code, "elastic_destroy_shared");
        assert_eq!(
            params,
            vec![("array".to_string(), "media".to_string()), ("shares".to_string(), "kadry, a&b=c ż".to_string())]
        );
        assert_eq!(text, "The array serves shares: a & b");
        let error: ProtocolError = refusal.into();
        assert_eq!(error.code, ProtocolErrorCode::Conflict);
        assert_eq!(error.message, wire);
    }

    /// The exact wires `www/js/modules/tentanas/refusal-wire.test.js` sends
    /// through the real client: the node's writer and the screen's reader are
    /// held to one string.
    #[test]
    fn the_wire_form_the_screens_read() {
        let member = Refusal::bad_request("elastic_disk_in_array", "data disk no. 2 is already in the array media")
            .disk(DiskWords::Data(2))
            .param("array", "media");
        assert_eq!(
            member.wire(),
            "refusal:elastic_disk_in_array?data=2&array=media Data disk no. 2 is already in the array media"
        );
        let shared = Refusal::not_available(
            "elastic_destroy_shared",
            "The array serves shares: kadry, zdjęcia & wideo. Delete them before dissolving the array",
        )
        .param("array", "media")
        .param("shares", "kadry, zdjęcia & wideo");
        assert_eq!(
            shared.wire(),
            "refusal:elastic_destroy_shared?array=media&shares=kadry%2C%20zdj%C4%99cia%20%26%20wideo \
             The array serves shares: kadry, zdjęcia & wideo. Delete them before dissolving the array"
        );
    }

    #[test]
    fn a_refusal_is_found_through_an_anyhow_chain() {
        let error = anyhow::Error::new(Refusal::not_found("elastic_journal_gone", "gone"))
            .context("while adopting");
        assert_eq!(Refusal::find(&error).map(|r| r.code), Some("elastic_journal_gone"));
        assert!(Refusal::find(&anyhow::anyhow!("plain")).is_none());
    }

    #[test]
    fn a_member_is_named_by_kernel_name_then_by_its_place_never_by_its_slot() {
        assert_eq!(DiskWords::member("d2", "sdh"), DiskWords::Kernel("sdh".into()));
        assert_eq!(DiskWords::member("d2", ""), DiskWords::Data(2));
        assert_eq!(DiskWords::member("parity1", ""), DiskWords::Parity(1));
        assert_eq!(DiskWords::member("c1", ""), DiskWords::Cache);
        assert_eq!(DiskWords::member("wwn-0x5000", ""), DiskWords::Unnamed);
        let wire = Refusal::not_available("x", "").disk(DiskWords::member("d3", "")).wire();
        assert_eq!(wire, "refusal:x?data=3");
    }
}
