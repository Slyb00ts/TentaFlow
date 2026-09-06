// ===== File: error.rs — crate error type and source positions =====

use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

/// 1-based position inside the OpenQASM 3 source that a diagnostic points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourcePos {
    pub line: u32,
    pub column: u32,
}

impl SourcePos {
    /// Translate a byte offset produced by the parser into a line/column pair.
    /// Columns count UTF-8 characters, not bytes, so a diagnostic on a line
    /// holding `π` still points at the right character in the editor.
    pub fn from_offset(source: &str, offset: usize) -> SourcePos {
        let offset = offset.min(source.len());
        let head = &source[..offset];
        let line = head.matches('\n').count() as u32 + 1;
        let line_start = head.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let column = source[line_start..offset].chars().count() as u32 + 1;
        SourcePos { line, column }
    }
}

impl fmt::Display for SourcePos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}", self.line, self.column)
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("{pos}: syntax error: {message}")]
    Syntax { pos: SourcePos, message: String },

    #[error("{pos}: {message}")]
    Semantic { pos: SourcePos, message: String },

    #[error("{pos}: `{construct}` is outside the supported OpenQASM 3 subset")]
    Unsupported { pos: SourcePos, construct: String },

    /// The OpenQASM 3 parser panics on a handful of constructs it declares
    /// unsupported (binary logic operators, for one). A panic must not take the
    /// process down when the input is a user-supplied circuit.
    #[error("the OpenQASM 3 parser rejected this program: {message}")]
    ParserPanic { message: String },

    #[error("`input float {name}` has no value bound for this run")]
    UnboundInput { name: String },

    #[error("{0}")]
    Invalid(String),

    #[error("circuit needs {qubits} qubits, this simulator allows at most {limit}")]
    TooManyQubits { qubits: usize, limit: usize },

    #[error("circuit is not Clifford: {reason}")]
    NotClifford { reason: String },

    /// A device backend could not be opened: no adapter answered, or the one
    /// that did cannot run the kernels. The caller decides whether to fall back
    /// to the CPU or to report the machine as having no GPU target.
    #[error("the {device} backend is not available: {reason}")]
    DeviceUnavailable { device: String, reason: String },

    /// The caller's [`crate::sim::Cancel`] hook ended a shot loop. Why it did
    /// is the caller's business — a person cancelled, a deadline elapsed — so
    /// the crate reports only that it happened.
    #[error("run stopped at the caller's request")]
    Cancelled,
}

impl Error {
    /// Position of the diagnostic when it has one.
    pub fn position(&self) -> Option<SourcePos> {
        match self {
            Error::Syntax { pos, .. }
            | Error::Semantic { pos, .. }
            | Error::Unsupported { pos, .. } => Some(*pos),
            _ => None,
        }
    }
}

/// Shorthand for the many places that report a malformed program.
pub(crate) fn invalid<T: Into<String>>(message: T) -> Error {
    Error::Invalid(message.into())
}
