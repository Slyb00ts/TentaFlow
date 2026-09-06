// ===== File: code_studio/terminal.rs — the terminal's VT state lives on the server =====
//
// The browser does not receive a byte stream. It receives a GRID OF CELLS with
// a revision number, and sends keystrokes back. The VT100 machine runs here, on
// the owner node, which is what makes four properties true at once: the terminal
// behaves identically whether the shell runs locally or in a container, the
// scrollback survives a page reload and a Core restart, a reconnecting client
// asks for "everything after revision N" instead of replaying a stream, and the
// mesh carries changed cells rather than raw output (§7.9).
//
// The parser is written here rather than taken from the `vt100` crate because
// `tentaflow-core` does not depend on it and this change does not add
// dependencies. It covers what a terminal session actually produces: UTF-8
// text, cursor movement, erase, insert/delete, scroll regions, SGR colour and
// attributes, autowrap, tab stops, the alternate screen and OSC (consumed and
// ignored). One deliberate simplification: the grid is one character per cell,
// so a combining mark occupies a cell of its own; a double-width character
// takes two, the second marked as a continuation.
//
// Two rules that are not about rendering:
//
// **No terminal in `plan` mode.** The mode forbids execution, and a terminal is
// execution with a nicer surface. The refusal lives in this API, not in the UI,
// because the binary protocol is reachable without the UI.
//
// **Every shell start is an `exec` event.** `TerminalOpened` carries the argv
// element by element so the redactor can replace a single element before the
// event is written (§13.4) — a terminal is exactly where a person pastes a
// token by mistake.
//
// **Output is scrubbed before it reaches a cell, and over LOGICAL lines.** A
// terminal is where a person pastes a token by mistake and where `cat id_ed25519`
// prints a private key, and the grid is not a view: it is streamed to the
// dashboard, streamed over the mesh and flushed to an artifact on disk once it
// passes the inline budget (§13.4 names "command output" explicitly).
//
// Redacting row by row would not be redaction. At 80 columns an autowrapped
// 40-character token is split across two `GridRow`s, and neither half is long
// enough for the entropy rule to fire. The scrub therefore runs over the
// LOGICAL line — a row plus every row it wrapped into — and masks the cells the
// span covers, in place, one mask character per original cell. Two consequences
// were chosen deliberately:
//
// * The mask keeps the WIDTH of what it replaces. The program on the other end
//   believes it wrote those columns; changing how many there are would move
//   everything after them.
// * It runs inside `feed`, under the same lock every reader takes, so there is
//   no revision in which a complete credential is visible to a snapshot. The
//   alternative — holding output back until a separator arrives — would delay
//   the echo of every word being typed, which is not a terminal.
//
// A PEM block is the one credential that sits in no value position at all, so
// it is handled by its delimiters, with the "inside a key" state carried across
// lines and across scrollback eviction.
//
// Defect D2 of §1.2 is fixed by the orphan record: a terminal writes its pid to
// disk while it runs, so after a crash the next start finds it, kills it and
// verifies it is gone. A process that is merely forgotten is not "closed" — and
// a pid is not an identity, so the record also carries what makes the process
// THAT process, and a pid that has been recycled is never killed.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::exec::{self, ExecEnv};
use super::models::AutonomyMode;
use super::paths;
use super::redact;
use super::sandbox::ExecTarget;

/// The pseudo-terminal backend of this platform.
#[cfg(unix)]
use super::exec::unix as backend;

/// Windows has no backend in this build: a ConPTY needs `portable-pty`, which
/// `tentaflow-core` does not link. Every entry point says so rather than
/// returning a handle that would never produce output, and `pty_open` — the
/// only way to obtain a handle — fails first, so nothing below it is reachable.
#[cfg(not(unix))]
mod backend {
    use std::io;
    use std::path::Path;

    pub struct PtyChild {
        pub master: i32,
        pub pid: i32,
    }

    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the server-side terminal needs a ConPTY backend (crate `portable-pty`), which this \
             build does not link",
        ))
    }

    pub fn open_pty(
        _argv: &[String],
        _env: &[(String, String)],
        _cwd: &Path,
        _rows: u16,
        _cols: u16,
    ) -> io::Result<PtyChild> {
        unsupported()
    }

    pub fn read(_master: i32, _buf: &mut [u8]) -> io::Result<usize> {
        unsupported()
    }

    pub fn write(_master: i32, _buf: &[u8]) -> io::Result<usize> {
        unsupported()
    }

    pub fn resize(_master: i32, _rows: u16, _cols: u16) -> io::Result<()> {
        unsupported()
    }

    pub fn close(_master: i32) {}

    pub fn kill_and_reap(pid: i32) -> bool {
        !process_alive(pid)
    }

    pub fn process_alive(pid: i32) -> bool {
        #[cfg(windows)]
        {
            super::super::exec::windows::process_alive(pid)
        }
        #[cfg(not(windows))]
        {
            let _ = pid;
            false
        }
    }
}

/// Lines kept above the visible screen. Server-side, so it outlives the page.
pub const SCROLLBACK_LIMIT: usize = 2000;

const DEFAULT_ROWS: u16 = 24;
const DEFAULT_COLS: u16 = 80;
const MAX_ROWS: u16 = 300;
const MAX_COLS: u16 = 500;

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    /// `plan` mode has no execution, so it has no terminal either.
    #[error("a terminal is not available in plan mode")]
    NotAvailableInPlanMode,
    #[error("no terminal {0}")]
    Unknown(String),
    #[error("terminal {0} has already been closed")]
    Closed(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// ---------------------------------------------------------------------------
// Grid
// ---------------------------------------------------------------------------

/// A colour as the sequence expressed it. Resolving the palette is the
/// renderer's job — storing an index keeps a theme change from rewriting
/// scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub mod attrs {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const BLINK: u16 = 1 << 4;
    pub const REVERSE: u16 = 1 << 5;
    pub const HIDDEN: u16 = 1 << 6;
    pub const STRIKE: u16 = 1 << 7;
}

/// One cell. `ch == '\0'` marks the second half of a double-width character —
/// a renderer skips it, and a copy of the line skips it too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            attrs: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
}

/// One row, tagged with the revision at which it last changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridRow {
    pub index: u16,
    pub revision: u64,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalGrid {
    pub revision: u64,
    pub rows: u16,
    pub cols: u16,
    pub cursor: Cursor,
    pub lines: Vec<GridRow>,
}

/// Rows that changed after `since`. Everything else the client already has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalChanges {
    pub revision: u64,
    pub rows: u16,
    pub cols: u16,
    pub cursor: Cursor,
    pub lines: Vec<GridRow>,
}

// ---------------------------------------------------------------------------
// VT machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserState {
    Ground,
    Escape,
    /// `ESC (`, `ESC )` … — a charset designation whose next byte is consumed.
    EscapeIntermediate,
    Csi,
    /// A CSI with more parameters than are worth keeping; consumed to its end.
    CsiIgnore,
    /// An operating-system command, consumed until BEL or ST.
    Osc,
}

struct Screen {
    lines: Vec<Vec<Cell>>,
    revisions: Vec<u64>,
}

impl Screen {
    fn new(rows: usize, cols: usize, revision: u64) -> Self {
        Self {
            lines: vec![vec![Cell::default(); cols]; rows],
            revisions: vec![revision; rows],
        }
    }
}

/// The terminal state machine. Public so sequences can be tested directly,
/// without a process on the other end.
pub struct Vt {
    rows: usize,
    cols: usize,
    primary: Screen,
    alternate: Screen,
    on_alternate: bool,
    row: usize,
    col: usize,
    saved: Option<(usize, usize)>,
    fg: Color,
    bg: Color,
    attrs: u16,
    scroll_top: usize,
    scroll_bottom: usize,
    autowrap: bool,
    wrap_pending: bool,
    cursor_visible: bool,
    tab_stops: Vec<bool>,
    revision: u64,
    scrollback: VecDeque<Vec<Cell>>,
    state: ParserState,
    params: Vec<u32>,
    param_empty: bool,
    private: bool,
    utf8: Vec<u8>,
    utf8_needed: usize,
    /// Whether a PEM private-key block was still open at the row that is now at
    /// the top of the primary screen. Carried across scrollback eviction, so a
    /// key that scrolls past the top does not un-redact the rest of itself.
    pem_open: bool,
}

impl Vt {
    pub fn new(rows: u16, cols: u16) -> Self {
        let rows = rows.clamp(1, MAX_ROWS) as usize;
        let cols = cols.clamp(1, MAX_COLS) as usize;
        Self {
            rows,
            cols,
            primary: Screen::new(rows, cols, 1),
            alternate: Screen::new(rows, cols, 1),
            on_alternate: false,
            row: 0,
            col: 0,
            saved: None,
            fg: Color::Default,
            bg: Color::Default,
            attrs: 0,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            autowrap: true,
            wrap_pending: false,
            cursor_visible: true,
            tab_stops: default_tab_stops(cols),
            revision: 1,
            scrollback: VecDeque::new(),
            state: ParserState::Ground,
            params: Vec::new(),
            param_empty: true,
            private: false,
            utf8: Vec::new(),
            utf8_needed: 0,
            pem_open: false,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows as u16, self.cols as u16)
    }

    pub fn scrollback(&self) -> &VecDeque<Vec<Cell>> {
        &self.scrollback
    }

    /// Applies one chunk of terminal output. The revision advances ONCE per
    /// chunk and every row the chunk touched is stamped with it, which is what
    /// makes `changes_since` return whole rows and nothing else.
    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.revision += 1;
        for &byte in bytes {
            self.consume(byte);
        }
        // Before anything can read the grid: the lock this call holds is the
        // same one `snapshot` and `changes_since` take, so no revision in which
        // a credential is complete is ever observable.
        self.scrub_screen();
    }

    /// Masks every credential visible on the current screen.
    fn scrub_screen(&mut self) {
        let start = if self.on_alternate {
            false
        } else {
            self.pem_open
        };
        // The carried state belongs to the TOP row and only eviction moves it;
        // the state further down is recomputed from it on every pass, so the
        // result of this scan is not kept.
        self.scrub_rows(0, self.rows, start);
    }

    /// Scrubs the logical lines starting inside `[start, end)`, carrying the
    /// PEM state forward. Returns the state after the last line.
    fn scrub_rows(&mut self, start: usize, end: usize, mut open: bool) -> bool {
        let mut row = start;
        while row < end {
            let (text, cells, next) = self.logical_line(row);
            open = self.scrub_logical_line(&text, &cells, open);
            row = next;
        }
        open
    }

    /// One logical line: a row plus every row it wrapped into. Returns the text,
    /// the cell each character came from, and the row after the line.
    ///
    /// A row is treated as wrapped when its last cell is not blank, which is
    /// exactly the condition under which `print` wrapped. A line that happens to
    /// fill the last column and then ended is joined with the next one, which
    /// can only widen what the scrubber looks at — never narrow it.
    fn logical_line(&self, start: usize) -> (String, Vec<(usize, usize, usize)>, usize) {
        let screen = self.screen();
        let mut text = String::new();
        let mut cells: Vec<(usize, usize, usize)> = Vec::new();
        let mut row = start;
        while row < self.rows {
            let line = &screen.lines[row];
            for (col, cell) in line.iter().enumerate() {
                if cell.ch == '\0' {
                    continue;
                }
                cells.push((row, col, text.len()));
                text.push(cell.ch);
            }
            row += 1;
            let wrapped = line.last().is_some_and(|cell| cell.ch != ' ');
            if !wrapped {
                break;
            }
        }
        (text, cells, row)
    }

    /// Applies the scrubber to one logical line. Returns whether a PEM block is
    /// open after it.
    fn scrub_logical_line(
        &mut self,
        text: &str,
        cells: &[(usize, usize, usize)],
        open: bool,
    ) -> bool {
        match redact::private_key_marker(text) {
            // The delimiters are not the secret, and keeping them is what makes
            // the redaction readable — and what carries the state.
            Some(redact::PrivateKeyMarker::Begin) => return true,
            Some(redact::PrivateKeyMarker::End) => return false,
            None => {}
        }
        if open {
            let body: Vec<(usize, usize)> = cells
                .iter()
                .filter(|(row, col, _)| self.screen().lines[*row][*col].ch != ' ')
                .map(|(row, col, _)| (*row, *col))
                .collect();
            self.mask_cells(&body);
            return true;
        }
        for span in redact::secret_spans(text) {
            let covered: Vec<(usize, usize)> = cells
                .iter()
                .filter(|(_, _, offset)| span.contains(offset))
                .map(|(row, col, _)| (*row, *col))
                .collect();
            self.mask_cells(&covered);
        }
        false
    }

    /// Overwrites a run of cells with the redaction marker, keeping the cell
    /// count and every attribute the cells carried.
    fn mask_cells(&mut self, cells: &[(usize, usize)]) {
        if cells.is_empty() {
            return;
        }
        let marker: Vec<char> = redact::REDACTED.chars().collect();
        let revision = self.revision;
        let cols = self.cols;
        let screen = self.screen_mut();
        for (index, (row, col)) in cells.iter().enumerate() {
            let ch = marker.get(index).copied().unwrap_or(' ');
            let line = &mut screen.lines[*row];
            line[*col].ch = ch;
            // The second half of a double-width character has just lost the
            // character it belonged to; leaving `\0` there would make the
            // renderer skip a column that now holds the mask.
            if *col + 1 < cols && line[*col + 1].ch == '\0' {
                line[*col + 1].ch = ' ';
            }
            screen.revisions[*row] = revision;
        }
    }

    pub fn cursor(&self) -> Cursor {
        Cursor {
            row: self.row.min(self.rows - 1) as u16,
            col: self.col.min(self.cols - 1) as u16,
            visible: self.cursor_visible,
        }
    }

    pub fn snapshot(&self) -> TerminalGrid {
        let screen = self.screen();
        TerminalGrid {
            revision: self.revision,
            rows: self.rows as u16,
            cols: self.cols as u16,
            cursor: self.cursor(),
            lines: (0..self.rows)
                .map(|index| GridRow {
                    index: index as u16,
                    revision: screen.revisions[index],
                    cells: screen.lines[index].clone(),
                })
                .collect(),
        }
    }

    pub fn changes_since(&self, since: u64) -> TerminalChanges {
        let screen = self.screen();
        TerminalChanges {
            revision: self.revision,
            rows: self.rows as u16,
            cols: self.cols as u16,
            cursor: self.cursor(),
            lines: (0..self.rows)
                .filter(|index| screen.revisions[*index] > since)
                .map(|index| GridRow {
                    index: index as u16,
                    revision: screen.revisions[index],
                    cells: screen.lines[index].clone(),
                })
                .collect(),
        }
    }

    /// A resize rewrites every row, so the whole screen is marked changed —
    /// a client that diffs by row would otherwise keep a stale line width.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.clamp(1, MAX_ROWS) as usize;
        let cols = cols.clamp(1, MAX_COLS) as usize;
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.revision += 1;
        let revision = self.revision;
        for screen in [&mut self.primary, &mut self.alternate] {
            screen.lines.resize(rows, vec![Cell::default(); cols]);
            for line in screen.lines.iter_mut() {
                line.resize(cols, Cell::default());
            }
            screen.revisions.resize(rows, 0);
            for slot in screen.revisions.iter_mut() {
                *slot = revision;
            }
        }
        self.rows = rows;
        self.cols = cols;
        self.tab_stops = default_tab_stops(cols);
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.row = self.row.min(rows - 1);
        self.col = self.col.min(cols - 1);
        self.wrap_pending = false;
    }

    fn screen(&self) -> &Screen {
        if self.on_alternate {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn screen_mut(&mut self) -> &mut Screen {
        if self.on_alternate {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    fn mark(&mut self, row: usize) {
        let revision = self.revision;
        if let Some(slot) = self.screen_mut().revisions.get_mut(row) {
            *slot = revision;
        }
    }

    fn consume(&mut self, byte: u8) {
        match self.state {
            ParserState::Ground => self.ground(byte),
            ParserState::Escape => self.escape(byte),
            ParserState::EscapeIntermediate => self.state = ParserState::Ground,
            ParserState::Csi => self.csi(byte),
            ParserState::CsiIgnore => {
                if (0x40..=0x7e).contains(&byte) {
                    self.state = ParserState::Ground;
                }
            }
            ParserState::Osc => {
                // BEL ends it; ESC starts the two-byte ST, whose second byte is
                // swallowed by the escape handler.
                if byte == 0x07 {
                    self.state = ParserState::Ground;
                } else if byte == 0x1b {
                    self.state = ParserState::Escape;
                }
            }
        }
    }

    fn ground(&mut self, byte: u8) {
        if self.utf8_needed > 0 {
            if byte & 0xc0 == 0x80 {
                self.utf8.push(byte);
                self.utf8_needed -= 1;
                if self.utf8_needed == 0 {
                    let text = String::from_utf8_lossy(&self.utf8).into_owned();
                    self.utf8.clear();
                    for ch in text.chars() {
                        self.print(ch);
                    }
                }
                return;
            }
            // A broken sequence: emit the replacement and reprocess this byte
            // as the start of something new, rather than swallowing both.
            self.utf8.clear();
            self.utf8_needed = 0;
            self.print('\u{fffd}');
        }

        match byte {
            0x07 => {}
            0x08 => self.backspace(),
            0x09 => self.tab(),
            0x0a | 0x0b | 0x0c => {
                self.wrap_pending = false;
                self.index();
            }
            0x0d => {
                self.wrap_pending = false;
                self.col = 0;
            }
            0x1b => {
                self.state = ParserState::Escape;
                self.params.clear();
                self.param_empty = true;
                self.private = false;
            }
            0x00..=0x1f | 0x7f => {}
            0x20..=0x7e => self.print(byte as char),
            0xc0..=0xdf => {
                self.utf8.clear();
                self.utf8.push(byte);
                self.utf8_needed = 1;
            }
            0xe0..=0xef => {
                self.utf8.clear();
                self.utf8.push(byte);
                self.utf8_needed = 2;
            }
            0xf0..=0xf7 => {
                self.utf8.clear();
                self.utf8.push(byte);
                self.utf8_needed = 3;
            }
            _ => self.print('\u{fffd}'),
        }
    }

    fn escape(&mut self, byte: u8) {
        self.state = ParserState::Ground;
        match byte {
            b'[' => {
                self.state = ParserState::Csi;
                self.params.clear();
                self.param_empty = true;
                self.private = false;
            }
            b']' => self.state = ParserState::Osc,
            b'7' => self.saved = Some((self.row, self.col)),
            b'8' => {
                if let Some((row, col)) = self.saved {
                    self.row = row.min(self.rows - 1);
                    self.col = col.min(self.cols - 1);
                    self.wrap_pending = false;
                }
            }
            b'D' => self.index(),
            b'M' => self.reverse_index(),
            b'E' => {
                self.col = 0;
                self.index();
            }
            b'c' => self.reset(),
            b'(' | b')' | b'*' | b'+' | b'%' | b'#' => self.state = ParserState::EscapeIntermediate,
            b'\\' | b'=' | b'>' => {}
            _ => {}
        }
    }

    fn csi(&mut self, byte: u8) {
        match byte {
            b'0'..=b'9' => {
                if self.param_empty {
                    self.params.push(0);
                    self.param_empty = false;
                }
                if let Some(last) = self.params.last_mut() {
                    *last = last
                        .saturating_mul(10)
                        .saturating_add(u32::from(byte - b'0'));
                }
            }
            // `:` separates sub-parameters (`38:5:n`). Treating it as `;` makes
            // the extended colour forms parse identically, which is the only
            // place either separator appears in practice.
            b';' | b':' => {
                if self.param_empty {
                    self.params.push(0);
                }
                self.param_empty = true;
            }
            b'?' | b'<' | b'=' | b'>' => self.private = true,
            0x20..=0x2f => {}
            // A missing parameter is not zero: what it means is command
            // specific, which is what `param(index, default)` resolves.
            0x40..=0x7e => {
                self.state = ParserState::Ground;
                self.dispatch_csi(byte);
            }
            _ => self.state = ParserState::CsiIgnore,
        }
    }

    fn param(&self, index: usize, default: u32) -> u32 {
        match self.params.get(index) {
            Some(0) | None => default,
            Some(value) => *value,
        }
    }

    fn dispatch_csi(&mut self, final_byte: u8) {
        match final_byte {
            b'A' => self.move_up(self.param(0, 1) as usize),
            b'B' => self.move_down(self.param(0, 1) as usize),
            b'C' => self.move_right(self.param(0, 1) as usize),
            b'D' => self.move_left(self.param(0, 1) as usize),
            b'E' => {
                self.col = 0;
                self.move_down(self.param(0, 1) as usize);
            }
            b'F' => {
                self.col = 0;
                self.move_up(self.param(0, 1) as usize);
            }
            b'G' | b'`' => {
                self.col = (self.param(0, 1) as usize - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            b'd' => {
                self.row = (self.param(0, 1) as usize - 1).min(self.rows - 1);
                self.wrap_pending = false;
            }
            b'H' | b'f' => {
                self.row = (self.param(0, 1) as usize - 1).min(self.rows - 1);
                self.col = (self.param(1, 1) as usize - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            b'J' => self.erase_display(self.params.first().copied().unwrap_or(0)),
            b'K' => self.erase_line(self.params.first().copied().unwrap_or(0)),
            b'L' => self.insert_lines(self.param(0, 1) as usize),
            b'M' => self.delete_lines(self.param(0, 1) as usize),
            b'P' => self.delete_chars(self.param(0, 1) as usize),
            b'@' => self.insert_chars(self.param(0, 1) as usize),
            b'X' => self.erase_chars(self.param(0, 1) as usize),
            b'S' => self.scroll_up(self.param(0, 1) as usize),
            b'T' => self.scroll_down(self.param(0, 1) as usize),
            b'r' => {
                let top = self.param(0, 1) as usize - 1;
                let bottom = self.param(1, self.rows as u32) as usize - 1;
                if top < bottom && bottom < self.rows {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                    self.row = top;
                    self.col = 0;
                    self.wrap_pending = false;
                }
            }
            b'g' => {
                match self.params.first().copied().unwrap_or(0) {
                    3 => self.tab_stops.iter_mut().for_each(|stop| *stop = false),
                    _ => {
                        if let Some(stop) = self.tab_stops.get_mut(self.col) {
                            *stop = false;
                        }
                    }
                };
            }
            b'm' => self.select_graphic_rendition(),
            b'h' => self.set_mode(true),
            b'l' => self.set_mode(false),
            b's' => self.saved = Some((self.row, self.col)),
            b'u' => {
                if let Some((row, col)) = self.saved {
                    self.row = row.min(self.rows - 1);
                    self.col = col.min(self.cols - 1);
                }
            }
            _ => {}
        }
    }

    fn set_mode(&mut self, enable: bool) {
        if !self.private {
            return;
        }
        for index in 0..self.params.len().max(1) {
            match self.params.get(index).copied().unwrap_or(0) {
                7 => {
                    self.autowrap = enable;
                    self.wrap_pending = false;
                }
                25 => self.cursor_visible = enable,
                47 | 1047 | 1049 => self.switch_screen(enable),
                _ => {}
            }
        }
    }

    /// The alternate screen is a separate grid, which is why a full-screen
    /// editor does not shred the scrollback of the shell underneath it.
    fn switch_screen(&mut self, alternate: bool) {
        if self.on_alternate == alternate {
            return;
        }
        if alternate {
            self.saved = Some((self.row, self.col));
            self.alternate = Screen::new(self.rows, self.cols, self.revision);
            self.on_alternate = true;
            self.row = 0;
            self.col = 0;
        } else {
            self.on_alternate = false;
            if let Some((row, col)) = self.saved.take() {
                self.row = row.min(self.rows - 1);
                self.col = col.min(self.cols - 1);
            }
        }
        let revision = self.revision;
        for slot in self.screen_mut().revisions.iter_mut() {
            *slot = revision;
        }
    }

    fn select_graphic_rendition(&mut self) {
        if self.params.is_empty() {
            self.fg = Color::Default;
            self.bg = Color::Default;
            self.attrs = 0;
            return;
        }
        let params = self.params.clone();
        let mut index = 0;
        while index < params.len() {
            match params[index] {
                0 => {
                    self.fg = Color::Default;
                    self.bg = Color::Default;
                    self.attrs = 0;
                }
                1 => self.attrs |= attrs::BOLD,
                2 => self.attrs |= attrs::DIM,
                3 => self.attrs |= attrs::ITALIC,
                4 => self.attrs |= attrs::UNDERLINE,
                5 | 6 => self.attrs |= attrs::BLINK,
                7 => self.attrs |= attrs::REVERSE,
                8 => self.attrs |= attrs::HIDDEN,
                9 => self.attrs |= attrs::STRIKE,
                21 | 22 => self.attrs &= !(attrs::BOLD | attrs::DIM),
                23 => self.attrs &= !attrs::ITALIC,
                24 => self.attrs &= !attrs::UNDERLINE,
                25 => self.attrs &= !attrs::BLINK,
                27 => self.attrs &= !attrs::REVERSE,
                28 => self.attrs &= !attrs::HIDDEN,
                29 => self.attrs &= !attrs::STRIKE,
                30..=37 => self.fg = Color::Indexed((params[index] - 30) as u8),
                39 => self.fg = Color::Default,
                40..=47 => self.bg = Color::Indexed((params[index] - 40) as u8),
                49 => self.bg = Color::Default,
                90..=97 => self.fg = Color::Indexed((params[index] - 90 + 8) as u8),
                100..=107 => self.bg = Color::Indexed((params[index] - 100 + 8) as u8),
                38 | 48 => {
                    let target_is_fg = params[index] == 38;
                    let (color, consumed) = extended_color(&params[index + 1..]);
                    if let Some(color) = color {
                        if target_is_fg {
                            self.fg = color;
                        } else {
                            self.bg = color;
                        }
                    }
                    index += consumed;
                }
                _ => {}
            }
            index += 1;
        }
    }

    fn print(&mut self, ch: char) {
        // A double-width character on a one-column terminal has nowhere to put
        // its continuation cell, so it occupies the single column it has.
        let width = char_width(ch).min(self.cols);
        if self.wrap_pending {
            self.wrap_pending = false;
            if self.autowrap {
                self.col = 0;
                self.index();
            }
        }
        if self.col + width > self.cols {
            if self.autowrap {
                self.col = 0;
                self.index();
            } else {
                self.col = self.cols - width;
            }
        }
        let cell = Cell {
            ch,
            fg: self.fg,
            bg: self.bg,
            attrs: self.attrs,
        };
        let (row, col) = (self.row, self.col);
        {
            let line = &mut self.screen_mut().lines[row];
            line[col] = cell;
            if width == 2 {
                line[col + 1] = Cell { ch: '\0', ..cell };
            }
        }
        self.mark(row);
        self.col += width;
        if self.col >= self.cols {
            self.col = self.cols - 1;
            self.wrap_pending = true;
        }
    }

    fn backspace(&mut self) {
        self.wrap_pending = false;
        self.col = self.col.saturating_sub(1);
    }

    fn tab(&mut self) {
        self.wrap_pending = false;
        let mut col = self.col + 1;
        while col < self.cols && !self.tab_stops[col] {
            col += 1;
        }
        self.col = col.min(self.cols - 1);
    }

    /// One line down, scrolling the region when the cursor is already at its
    /// bottom.
    fn index(&mut self) {
        if self.row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.row == self.scroll_top {
            self.scroll_down(1);
        } else {
            self.row = self.row.saturating_sub(1);
        }
    }

    fn scroll_up(&mut self, count: usize) {
        let (top, bottom) = (self.scroll_top, self.scroll_bottom);
        let count = count.min(bottom - top + 1);
        let keep_history = top == 0 && !self.on_alternate;
        let cols = self.cols;
        for _ in 0..count {
            if keep_history {
                // The last moment this line can be scrubbed: the end-of-chunk
                // pass only sees the visible screen, and a line that reaches the
                // scrollback with a credential on it stays there for 2000 lines.
                let open = self.pem_open;
                let (text, cells, _) = self.logical_line(top);
                self.pem_open = self.scrub_logical_line(&text, &cells, open);
            }
            let evicted = self.screen_mut().lines.remove(top);
            if keep_history {
                self.scrollback.push_back(evicted);
                while self.scrollback.len() > SCROLLBACK_LIMIT {
                    self.scrollback.pop_front();
                }
            }
            self.screen_mut()
                .lines
                .insert(bottom, vec![Cell::default(); cols]);
        }
        for row in top..=bottom {
            self.mark(row);
        }
    }

    fn scroll_down(&mut self, count: usize) {
        let (top, bottom) = (self.scroll_top, self.scroll_bottom);
        let count = count.min(bottom - top + 1);
        let cols = self.cols;
        for _ in 0..count {
            self.screen_mut().lines.remove(bottom);
            self.screen_mut()
                .lines
                .insert(top, vec![Cell::default(); cols]);
        }
        for row in top..=bottom {
            self.mark(row);
        }
    }

    /// Cursor movement stops at the scroll region when the cursor is inside it,
    /// which is how a full-screen program keeps its header and footer intact.
    fn move_up(&mut self, count: usize) {
        self.wrap_pending = false;
        let limit = if self.row >= self.scroll_top {
            self.scroll_top
        } else {
            0
        };
        self.row = self.row.saturating_sub(count).max(limit);
    }

    fn move_down(&mut self, count: usize) {
        self.wrap_pending = false;
        let limit = if self.row <= self.scroll_bottom {
            self.scroll_bottom
        } else {
            self.rows - 1
        };
        self.row = (self.row + count).min(limit);
    }

    fn move_left(&mut self, count: usize) {
        self.wrap_pending = false;
        self.col = self.col.saturating_sub(count);
    }

    fn move_right(&mut self, count: usize) {
        self.wrap_pending = false;
        self.col = (self.col + count).min(self.cols - 1);
    }

    fn erase_display(&mut self, mode: u32) {
        let blank = self.blank();
        let (rows, cols) = (self.rows, self.cols);
        let (row, col) = (self.row, self.col);
        match mode {
            0 => {
                for index in col..cols {
                    self.screen_mut().lines[row][index] = blank;
                }
                self.mark(row);
                for line in row + 1..rows {
                    self.screen_mut().lines[line] = vec![blank; cols];
                    self.mark(line);
                }
            }
            1 => {
                for index in 0..=col.min(cols - 1) {
                    self.screen_mut().lines[row][index] = blank;
                }
                self.mark(row);
                for line in 0..row {
                    self.screen_mut().lines[line] = vec![blank; cols];
                    self.mark(line);
                }
            }
            _ => {
                for line in 0..rows {
                    self.screen_mut().lines[line] = vec![blank; cols];
                    self.mark(line);
                }
            }
        }
        self.wrap_pending = false;
    }

    fn erase_line(&mut self, mode: u32) {
        let blank = self.blank();
        let (row, col, cols) = (self.row, self.col, self.cols);
        let range = match mode {
            0 => col..cols,
            1 => 0..(col + 1).min(cols),
            _ => 0..cols,
        };
        for index in range {
            self.screen_mut().lines[row][index] = blank;
        }
        self.mark(row);
        self.wrap_pending = false;
    }

    fn erase_chars(&mut self, count: usize) {
        let blank = self.blank();
        let (row, col, cols) = (self.row, self.col, self.cols);
        for index in col..(col + count).min(cols) {
            self.screen_mut().lines[row][index] = blank;
        }
        self.mark(row);
    }

    fn insert_chars(&mut self, count: usize) {
        let blank = self.blank();
        let (row, col, cols) = (self.row, self.col, self.cols);
        {
            let line = &mut self.screen_mut().lines[row];
            for _ in 0..count.min(cols - col) {
                line.insert(col, blank);
                line.truncate(cols);
            }
        }
        self.mark(row);
    }

    fn delete_chars(&mut self, count: usize) {
        let blank = self.blank();
        let (row, col, cols) = (self.row, self.col, self.cols);
        {
            let line = &mut self.screen_mut().lines[row];
            for _ in 0..count.min(cols - col) {
                line.remove(col);
                line.push(blank);
            }
        }
        self.mark(row);
    }

    fn insert_lines(&mut self, count: usize) {
        if self.row < self.scroll_top || self.row > self.scroll_bottom {
            return;
        }
        let (row, bottom, cols) = (self.row, self.scroll_bottom, self.cols);
        let blank = self.blank();
        for _ in 0..count.min(bottom - row + 1) {
            self.screen_mut().lines.remove(bottom);
            self.screen_mut().lines.insert(row, vec![blank; cols]);
        }
        for line in row..=bottom {
            self.mark(line);
        }
    }

    fn delete_lines(&mut self, count: usize) {
        if self.row < self.scroll_top || self.row > self.scroll_bottom {
            return;
        }
        let (row, bottom, cols) = (self.row, self.scroll_bottom, self.cols);
        let blank = self.blank();
        for _ in 0..count.min(bottom - row + 1) {
            self.screen_mut().lines.remove(row);
            self.screen_mut().lines.insert(bottom, vec![blank; cols]);
        }
        for line in row..=bottom {
            self.mark(line);
        }
    }

    /// Erasing paints the CURRENT background, which is how a full-screen
    /// program fills the window with its own colour.
    fn blank(&self) -> Cell {
        Cell {
            ch: ' ',
            fg: self.fg,
            bg: self.bg,
            attrs: self.attrs & (attrs::REVERSE),
        }
    }

    fn reset(&mut self) {
        let revision = self.revision;
        self.primary = Screen::new(self.rows, self.cols, revision);
        self.alternate = Screen::new(self.rows, self.cols, revision);
        self.on_alternate = false;
        self.row = 0;
        self.col = 0;
        self.fg = Color::Default;
        self.bg = Color::Default;
        self.attrs = 0;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.autowrap = true;
        self.wrap_pending = false;
        self.cursor_visible = true;
        self.tab_stops = default_tab_stops(self.cols);
        self.pem_open = false;
    }
}

fn default_tab_stops(cols: usize) -> Vec<bool> {
    (0..cols).map(|col| col % 8 == 0 && col > 0).collect()
}

/// `38;5;n` and `38;2;r;g;b`. Returns the colour and how many extra parameters
/// it consumed.
fn extended_color(rest: &[u32]) -> (Option<Color>, usize) {
    match rest.first() {
        Some(5) => match rest.get(1) {
            Some(index) => (Some(Color::Indexed(*index as u8)), 2),
            None => (None, 1),
        },
        Some(2) => match (rest.get(1), rest.get(2), rest.get(3)) {
            (Some(r), Some(g), Some(b)) => (Some(Color::Rgb(*r as u8, *g as u8, *b as u8)), 4),
            _ => (None, rest.len()),
        },
        _ => (None, 0),
    }
}

/// Cell width of a character. Two for the ranges that are double width in every
/// terminal font; one for everything else, including combining marks — the grid
/// holds one character per cell, so a mark takes a cell rather than merging
/// into its neighbour.
fn char_width(ch: char) -> usize {
    let code = ch as u32;
    let wide = matches!(code,
        0x1100..=0x115f
        | 0x2e80..=0x303e
        | 0x3041..=0x33ff
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xa000..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe6f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f
        | 0x1f900..=0x1f9ff
        | 0x20000..=0x3fffd);
    if wide {
        2
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// Terminals
// ---------------------------------------------------------------------------

/// Reference to an open terminal. Carries the session so a caller cannot reach
/// another session's terminal by guessing an id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyHandle {
    pub terminal_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone)]
pub struct PtyOpen {
    pub terminal_id: String,
    pub session_id: String,
    pub rows: u16,
    pub cols: u16,
    /// Explicit shell argv. `None` picks the node's interactive shell.
    pub shell: Option<Vec<String>>,
}

/// The `exec` event a terminal start produces. The argv is STRUCTURAL: the
/// redactor replaces elements, and nothing here joins them into a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalExec {
    pub terminal_id: String,
    pub session_id: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub started_at: String,
}

#[derive(Debug, Clone)]
pub struct TerminalOpened {
    pub handle: PtyHandle,
    pub exec: TerminalExec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalState {
    Running,
    /// The shell exited on its own; the process has been waited for.
    Exited,
    /// Closed by us: the group was killed and reaped.
    Reaped,
}

/// What a startup reap found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapedTerminal {
    pub terminal_id: String,
    pub session_id: String,
    pub pid: i32,
    pub state: TerminalState,
}

#[derive(Debug, Serialize, Deserialize)]
struct OrphanRecord {
    terminal_id: String,
    session_id: String,
    pid: i32,
    /// The argv that was SPAWNED, runtime wrapper included — not the shell argv
    /// of the audit event. This record exists to identify a process, and a
    /// `docker exec` prefix is part of what identifies it.
    argv: Vec<String>,
    started_at: String,
    /// What makes this pid THAT process: on Linux the kernel's own start-time
    /// stamp, elsewhere the start time and command line as `ps` reports them.
    /// `None` means the platform could not tell us — and then nothing is killed.
    #[serde(default)]
    identity: Option<String>,
}

/// A token that distinguishes a live process from a later one that inherited
/// its pid. Pids are recycled within hours on a busy machine and within seconds
/// after a reboot, so "kill the pid we wrote down" is a way to kill somebody
/// else's process group.
#[cfg(target_os = "linux")]
fn process_identity(pid: i32) -> Option<String> {
    // Field 22 of /proc/<pid>/stat is the start time in clock ticks since boot.
    // Together with the pid it is the classic unique process identity, and it
    // needs no clock arithmetic to compare.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    let start_time = after_comm.split_whitespace().nth(19)?;
    Some(format!("linux:{start_time}"))
}

#[cfg(target_os = "macos")]
fn process_identity(pid: i32) -> Option<String> {
    let (seconds, micros) = super::process_sandbox::process_birthtime(pid).ok()?;
    (seconds != 0).then(|| format!("macos:{seconds}:{micros}"))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn process_identity(pid: i32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-o", "lstart=,args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!line.is_empty()).then(|| format!("ps:{line}"))
}

#[cfg(not(unix))]
fn process_identity(_pid: i32) -> Option<String> {
    None
}

struct Terminal {
    handle: PtyHandle,
    /// Master side of the pseudo-terminal. A plain descriptor number, which is
    /// what every platform's backend takes.
    master: i32,
    pid: i32,
    vt: Mutex<Vt>,
    state: Mutex<TerminalState>,
    closing: Arc<AtomicBool>,
    record: PathBuf,
    exec: TerminalExec,
}

/// Open terminals of one workspace, plus the on-disk record that makes a crash
/// recoverable.
pub struct TerminalRegistry {
    terminals: Mutex<HashMap<String, Arc<Terminal>>>,
    records_root: PathBuf,
}

impl TerminalRegistry {
    /// `records_root` is the workspace's `tmp/` directory: the record of a
    /// running terminal is runtime state of this node and must not be synced.
    pub fn new(records_root: impl Into<PathBuf>) -> Self {
        Self {
            terminals: Mutex::new(HashMap::new()),
            records_root: records_root.into(),
        }
    }

    /// Opens a terminal in a held sandbox. The caller passes the lease's target
    /// and environment (`ExecEnv::for_lease`) rather than the lease itself, so
    /// the terminal cannot outlive or reshape the sandbox it was given.
    ///
    /// `plan` mode is refused here rather than hidden in the UI, and the caller
    /// receives the `exec` event to journal — a terminal that started without
    /// leaving a trace would be the one execution path with no audit.
    pub fn pty_open(
        &self,
        autonomy: AutonomyMode,
        target: &ExecTarget,
        env: &ExecEnv,
        spec: &PtyOpen,
    ) -> std::result::Result<TerminalOpened, TerminalError> {
        if autonomy == AutonomyMode::Plan {
            return Err(TerminalError::NotAvailableInPlanMode);
        }
        validate_id(&spec.terminal_id).map_err(TerminalError::Other)?;
        paths::validate_session_id(&spec.session_id).map_err(TerminalError::Other)?;

        let shell = match &spec.shell {
            Some(argv) if !argv.is_empty() => argv.clone(),
            _ => interactive_shell(),
        };
        let plan = exec::pty_plan(target, &env.vars(Some("xterm-256color")), &shell)
            .map_err(TerminalError::Other)?;
        let rows = if spec.rows == 0 {
            DEFAULT_ROWS
        } else {
            spec.rows
        };
        let cols = if spec.cols == 0 {
            DEFAULT_COLS
        } else {
            spec.cols
        };

        let child = match backend::open_pty(&plan.argv, &plan.env, &plan.cwd, rows, cols) {
            Ok(child) => child,
            Err(error) => {
                super::process_sandbox::cancel_supervisor_launch(&plan.argv)
                    .map_err(TerminalError::Other)?;
                return Err(TerminalError::Other(anyhow!("cannot open a terminal: {error}")));
            }
        };

        let exec_event = TerminalExec {
            terminal_id: spec.terminal_id.clone(),
            session_id: spec.session_id.clone(),
            // The SHELL argv, not the runtime wrapper: what the person started
            // is the shell, and a `docker exec` prefix would only add noise a
            // reviewer has to skip.
            argv: shell,
            // The directory as it exists in the sandbox, not the host directory
            // a runtime client happened to be started from.
            cwd: match target {
                ExecTarget::Local { cwd } | ExecTarget::Process { cwd, .. } => {
                    cwd.display().to_string()
                }
                ExecTarget::Container { workdir, .. } => workdir.display().to_string(),
            },
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        let record = self.record_path(&spec.session_id, &spec.terminal_id);
        if let Err(e) = write_record(
            &record,
            &OrphanRecord {
                terminal_id: spec.terminal_id.clone(),
                session_id: spec.session_id.clone(),
                pid: child.pid,
                argv: plan.argv.clone(),
                started_at: exec_event.started_at.clone(),
                identity: process_identity(child.pid),
            },
        ) {
            // Without the record a crash would leave this shell running with
            // nothing pointing at it — exactly the defect the record exists to
            // prevent — so the terminal does not start at all.
            backend::kill_and_reap(child.pid);
            backend::close(child.master);
            return Err(TerminalError::Other(e));
        }

        let terminal = Arc::new(Terminal {
            handle: PtyHandle {
                terminal_id: spec.terminal_id.clone(),
                session_id: spec.session_id.clone(),
            },
            master: child.master,
            pid: child.pid,
            vt: Mutex::new(Vt::new(rows, cols)),
            state: Mutex::new(TerminalState::Running),
            closing: Arc::new(AtomicBool::new(false)),
            record,
            exec: exec_event.clone(),
        });
        self.terminals
            .lock()
            .map_err(|e| TerminalError::Other(anyhow!("terminal registry: {e}")))?
            .insert(spec.terminal_id.clone(), Arc::clone(&terminal));

        spawn_pump(Arc::clone(&terminal));
        Ok(TerminalOpened {
            handle: terminal.handle.clone(),
            exec: exec_event,
        })
    }

    /// Keystrokes. They go to the process, never to the grid: what appears on
    /// screen is whatever the program echoes back.
    pub fn pty_write(
        &self,
        handle: &PtyHandle,
        bytes: &[u8],
    ) -> std::result::Result<(), TerminalError> {
        let terminal = self.get(handle)?;
        if *terminal.state.lock().unwrap_or_else(|e| e.into_inner()) != TerminalState::Running {
            return Err(TerminalError::Closed(handle.terminal_id.clone()));
        }
        let mut written = 0;
        while written < bytes.len() {
            match backend::write(terminal.master, &bytes[written..]) {
                Ok(0) => break,
                Ok(n) => written += n,
                Err(e) => return Err(TerminalError::Other(anyhow!("terminal write: {e}"))),
            }
        }
        Ok(())
    }

    pub fn pty_resize(
        &self,
        handle: &PtyHandle,
        rows: u16,
        cols: u16,
    ) -> std::result::Result<(), TerminalError> {
        let terminal = self.get(handle)?;
        let rows = rows.clamp(1, MAX_ROWS);
        let cols = cols.clamp(1, MAX_COLS);
        // The grid first, the process second: a program that redraws on
        // SIGWINCH must not paint into a grid that is still the old size.
        terminal
            .vt
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resize(rows, cols);
        backend::resize(terminal.master, rows, cols)
            .map_err(|e| TerminalError::Other(anyhow!("terminal resize: {e}")))
    }

    /// Closes a terminal: the process group is killed and reaped, the master is
    /// closed and the on-disk record removed. `Reaped` is a fact here, not a
    /// hope — the state is only set after the process is gone.
    pub fn pty_close(
        &self,
        handle: &PtyHandle,
    ) -> std::result::Result<TerminalState, TerminalError> {
        let terminal = {
            let mut terminals = self
                .terminals
                .lock()
                .map_err(|e| TerminalError::Other(anyhow!("terminal registry: {e}")))?;
            // Ownership is checked BEFORE the removal: a handle carrying another
            // session's id must not be able to unregister a terminal it may not
            // even see.
            let owned = terminals
                .get(&handle.terminal_id)
                .is_some_and(|t| t.handle.session_id == handle.session_id);
            if !owned {
                return Err(TerminalError::Unknown(handle.terminal_id.clone()));
            }
            terminals
                .remove(&handle.terminal_id)
                .ok_or_else(|| TerminalError::Unknown(handle.terminal_id.clone()))?
        };
        terminal.closing.store(true, Ordering::SeqCst);
        let reaped = backend::kill_and_reap(terminal.pid);
        backend::close(terminal.master);
        let _ = std::fs::remove_file(&terminal.record);
        let state = if reaped {
            TerminalState::Reaped
        } else {
            TerminalState::Exited
        };
        *terminal.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
        Ok(state)
    }

    pub fn snapshot(&self, handle: &PtyHandle) -> std::result::Result<TerminalGrid, TerminalError> {
        let terminal = self.get(handle)?;
        let vt = terminal.vt.lock().unwrap_or_else(|e| e.into_inner());
        Ok(vt.snapshot())
    }

    pub fn changes_since(
        &self,
        handle: &PtyHandle,
        revision: u64,
    ) -> std::result::Result<TerminalChanges, TerminalError> {
        let terminal = self.get(handle)?;
        let vt = terminal.vt.lock().unwrap_or_else(|e| e.into_inner());
        Ok(vt.changes_since(revision))
    }

    pub fn state(&self, handle: &PtyHandle) -> std::result::Result<TerminalState, TerminalError> {
        let terminal = self.get(handle)?;
        let state = *terminal.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(state)
    }

    /// The `exec` event of an open terminal, for a caller re-journalling after
    /// a reconnect.
    pub fn exec_event(
        &self,
        handle: &PtyHandle,
    ) -> std::result::Result<TerminalExec, TerminalError> {
        Ok(self.get(handle)?.exec.clone())
    }

    pub fn session_handles(&self, session_id: &str) -> Vec<PtyHandle> {
        self.terminals
            .lock()
            .map(|terminals| {
                terminals
                    .values()
                    .filter(|t| t.handle.session_id == session_id)
                    .map(|t| t.handle.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Kills and reaps what a crash left behind. Called at startup, BEFORE any
    /// session is resumed: a shell from a previous life still holds the worktree
    /// and would happily keep writing to it (defect D2 of §1.2).
    ///
    /// A process inherited across a restart is not our child, so it cannot be
    /// waited for; what is verified instead is that it is gone.
    pub fn reap_orphans(&self) -> Result<Vec<ReapedTerminal>> {
        let mut reaped = Vec::new();
        let sessions = match std::fs::read_dir(&self.records_root) {
            Ok(entries) => entries,
            Err(_) => return Ok(reaped),
        };
        for session in sessions.flatten() {
            let dir = session.path().join("terminals");
            let Ok(records) = std::fs::read_dir(&dir) else {
                continue;
            };
            for record in records.flatten() {
                let path = record.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                match read_record(&path) {
                    Ok(orphan) => {
                        // A pid on its own is not an identity. If the process
                        // that answers to it now is not the one we started —
                        // recycled pid, or a record whose platform could not
                        // prove identity — it is left alone: killing a process
                        // GROUP by a stale pid takes somebody else's work with
                        // it, and after a reboot that is the normal case.
                        let live = process_identity(orphan.pid);
                        let ours = live.is_some() && live == orphan.identity;
                        let state = if !ours {
                            if live.is_some() {
                                warn!(
                                    pid = orphan.pid,
                                    terminal_id = %orphan.terminal_id,
                                    "terminal pid belongs to another process now; not killing it"
                                );
                            }
                            TerminalState::Exited
                        } else if backend::kill_and_reap(orphan.pid) {
                            TerminalState::Reaped
                        } else {
                            TerminalState::Exited
                        };
                        reaped.push(ReapedTerminal {
                            terminal_id: orphan.terminal_id,
                            session_id: orphan.session_id,
                            pid: orphan.pid,
                            state,
                        });
                    }
                    Err(e) => warn!("unreadable terminal record {}: {e:#}", path.display()),
                }
                let _ = std::fs::remove_file(&path);
            }
        }
        Ok(reaped)
    }

    fn get(&self, handle: &PtyHandle) -> std::result::Result<Arc<Terminal>, TerminalError> {
        let terminals = self
            .terminals
            .lock()
            .map_err(|e| TerminalError::Other(anyhow!("terminal registry: {e}")))?;
        terminals
            .get(&handle.terminal_id)
            // A terminal belongs to exactly one session; a handle carrying
            // another session's id is treated as unknown rather than refused,
            // so nothing leaks about which ids exist.
            .filter(|t| t.handle.session_id == handle.session_id)
            .cloned()
            .ok_or_else(|| TerminalError::Unknown(handle.terminal_id.clone()))
    }

    fn record_path(&self, session_id: &str, terminal_id: &str) -> PathBuf {
        self.records_root
            .join(session_id)
            .join("terminals")
            .join(format!("{terminal_id}.json"))
    }
}

/// Reads the master side until it ends, feeding the VT machine. One thread per
/// terminal: a terminal is a long-lived interactive thing, and a task per byte
/// chunk would buy nothing.
fn spawn_pump(terminal: Arc<Terminal>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match backend::read(terminal.master, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    terminal
                        .vt
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .feed(&buf[..n]);
                }
                Err(_) => break,
            }
        }
        if !terminal.closing.load(Ordering::SeqCst) {
            // The shell exited by itself. Reap it here so it cannot linger as a
            // zombie until someone closes the window.
            backend::kill_and_reap(terminal.pid);
            let _ = std::fs::remove_file(&terminal.record);
            *terminal.state.lock().unwrap_or_else(|e| e.into_inner()) = TerminalState::Exited;
        }
    });
}

/// The node's interactive shell. Derived, never taken from Core's own `SHELL`,
/// so the terminal a session gets does not depend on how the service happened
/// to be started.
#[cfg(unix)]
fn interactive_shell() -> Vec<String> {
    for candidate in ["/bin/bash", "/bin/sh"] {
        if Path::new(candidate).exists() {
            return vec![candidate.to_string(), "-i".to_string()];
        }
    }
    vec!["/bin/sh".to_string(), "-i".to_string()]
}

#[cfg(not(unix))]
fn interactive_shell() -> Vec<String> {
    vec!["cmd.exe".to_string()]
}

/// Same alphabet as every other id that reaches the filesystem.
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 64 {
        return Err(anyhow!("invalid terminal id"));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(anyhow!("invalid terminal id"));
    }
    Ok(())
}

fn write_record(path: &Path, record: &OrphanRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| anyhow!("terminal record directory: {e}"))?;
    }
    let encoded = serde_json::to_vec(record).map_err(|e| anyhow!("terminal record: {e}"))?;
    std::fs::write(path, encoded).map_err(|e| anyhow!("terminal record: {e}"))
}

fn read_record(path: &Path) -> Result<OrphanRecord> {
    let raw = std::fs::read(path).map_err(|e| anyhow!("read terminal record: {e}"))?;
    serde_json::from_slice(&raw).map_err(|e| anyhow!("parse terminal record: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything the terminal is holding: the visible grid AND the scrollback,
    /// which is what a reconnecting client and the mesh stream get.
    fn everything(vt: &Vt) -> String {
        let mut out = String::new();
        for line in vt.scrollback() {
            out.extend(line.iter().filter(|c| c.ch != '\0').map(|c| c.ch));
            out.push('\n');
        }
        let grid = vt.snapshot();
        for row in &grid.lines {
            out.extend(row.cells.iter().filter(|c| c.ch != '\0').map(|c| c.ch));
            out.push('\n');
        }
        out
    }

    /// A credential with no vendor prefix: only the entropy rule can catch it,
    /// so the test cannot pass on a shape match somewhere else.
    const PASTED: &str = "Ab3xK9mQ7pL2vR5tW8yZ1nC4jH6sD0fG";

    #[test]
    fn a_credential_typed_into_the_terminal_never_reaches_the_grid() {
        let mut vt = Vt::new(24, 80);
        vt.feed(format!("$ git push https://{PASTED}@github.com/org/repo.git\r\n").as_bytes());
        vt.feed(b"$ export API_TOKEN=Zx9WqT4nB7cM2vL5kR8jH3gF6dS1aP0e\r\n");
        let all = everything(&vt);
        assert!(!all.contains(PASTED), "{all}");
        assert!(!all.contains("Zx9WqT4nB7cM2vL5kR8jH3gF6dS1aP0e"), "{all}");
        assert!(
            all.contains("github.com/org/repo.git"),
            "the command is unreadable: {all}"
        );
        assert!(all.contains(redact::REDACTED), "no marker was left: {all}");
    }

    #[test]
    fn a_credential_split_by_the_right_margin_is_still_one_credential() {
        // 40 columns: the credential starts at column 19 and runs past the
        // margin, so no single `GridRow` holds enough of it for a per-row scrub
        // to fire. This is the reason the scrubber works on logical lines.
        let mut vt = Vt::new(6, 40);
        vt.feed(format!("$ git push https://{PASTED}@gh.example/r.git\r\n").as_bytes());
        let all = everything(&vt);
        assert!(!all.contains(PASTED), "{all}");
        for half in [&PASTED[..16], &PASTED[16..]] {
            assert!(!all.contains(half), "half of the token survived: {all}");
        }
    }

    #[test]
    fn a_private_key_printed_by_a_command_is_masked_including_its_body() {
        let mut vt = Vt::new(10, 80);
        vt.feed(
            b"$ cat id_ed25519\r\n\
              -----BEGIN OPENSSH PRIVATE KEY-----\r\n\
              b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAABlwAAAAdzc2gtcnNh\r\n\
              AAAAAwEAAQAAAYEAqvVvT2b9Yc0N1vTbGvCq7XlLzL5nTfP0Yy1kQmZmZmZmZmZmZmZmZmZm\r\n\
              -----END OPENSSH PRIVATE KEY-----\r\n\
              $ echo done\r\n",
        );
        let all = everything(&vt);
        assert!(!all.contains("b3BlbnNzaC1rZXktdjEA"), "{all}");
        assert!(!all.contains("AAAAAwEAAQAAAYEAqvVvT2b9"), "{all}");
        assert!(
            all.contains("-----BEGIN OPENSSH PRIVATE KEY-----"),
            "the marker is not the secret: {all}"
        );
        assert!(all.contains("$ echo done"), "output after the key was lost");
    }

    #[test]
    fn a_credential_that_scrolled_off_the_screen_is_redacted_in_the_scrollback() {
        // Four rows, so the credential leaves the visible screen inside the very
        // chunk that wrote it — the end-of-chunk pass never sees it again.
        let mut vt = Vt::new(4, 80);
        let mut stream = format!("$ export GIT_TOKEN={PASTED}\r\n");
        for index in 0..20 {
            stream.push_str(&format!("line {index}\r\n"));
        }
        vt.feed(stream.as_bytes());
        let all = everything(&vt);
        assert!(
            !all.contains(PASTED),
            "the scrollback kept the token: {all}"
        );
        assert!(all.contains("line 19"), "ordinary output was lost");
    }

    #[test]
    fn masking_keeps_the_geometry_the_program_believes_in() {
        let mut vt = Vt::new(4, 80);
        let line = format!("TOKEN={PASTED} && echo ok");
        vt.feed(format!("{line}\r\n").as_bytes());
        let grid = vt.snapshot();
        let row: String = grid.lines[0]
            .cells
            .iter()
            .filter(|c| c.ch != '\0')
            .map(|c| c.ch)
            .collect();
        assert_eq!(row.len(), 80, "the row changed width");
        assert!(row.starts_with("TOKEN="), "{row}");
        assert!(!row.contains(PASTED), "{row}");
        // The mask replaced the credential cell for cell, so everything the
        // program wrote after it is still in the column it wrote it to.
        let tail = line.find(" && echo ok").expect("fixture");
        assert!(
            row[tail..].starts_with(" && echo ok"),
            "the text after the credential moved: {row}"
        );
    }

    #[test]
    fn ordinary_output_is_left_alone() {
        let mut vt = Vt::new(24, 80);
        vt.feed(
            b"   Compiling tentaflow-core v0.1.0 (/mnt/d/repos/TentaFlow/tentaflow-core)\r\n\
              error[E0433]: failed to resolve: use of undeclared crate `serde_yaml`\r\n\
              $ cargo test --package tentaflow-core --lib code_studio\r\n",
        );
        let all = everything(&vt);
        assert!(all.contains("Compiling tentaflow-core v0.1.0"), "{all}");
        assert!(all.contains("use of undeclared crate"), "{all}");
        assert!(all.contains("--lib code_studio"), "{all}");
        assert!(
            !all.contains(redact::REDACTED),
            "build output was shredded: {all}"
        );
    }

    fn text(grid: &TerminalGrid, row: usize) -> String {
        grid.lines[row]
            .cells
            .iter()
            .filter(|cell| cell.ch != '\0')
            .map(|cell| cell.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn plain_text_lands_on_the_grid_with_the_cursor_after_it() {
        let mut vt = Vt::new(4, 10);
        vt.feed(b"hello");
        let grid = vt.snapshot();
        assert_eq!(text(&grid, 0), "hello");
        assert_eq!(
            grid.cursor,
            Cursor {
                row: 0,
                col: 5,
                visible: true
            }
        );
    }

    #[test]
    fn a_newline_is_a_line_feed_and_carriage_return_only_together() {
        let mut vt = Vt::new(4, 10);
        vt.feed(b"one\r\ntwo");
        let grid = vt.snapshot();
        assert_eq!(text(&grid, 0), "one");
        assert_eq!(text(&grid, 1), "two");

        // A bare line feed keeps the column, as a real terminal does.
        let mut bare = Vt::new(4, 10);
        bare.feed(b"one\ntwo");
        let grid = bare.snapshot();
        assert_eq!(text(&grid, 0), "one");
        assert_eq!(grid.lines[1].cells[3].ch, 't');
    }

    #[test]
    fn sgr_colours_and_attributes_stick_to_the_cells_they_were_set_before() {
        let mut vt = Vt::new(2, 20);
        vt.feed(b"\x1b[1;31mred\x1b[0m plain\x1b[38;2;10;20;30mrgb\x1b[m");
        let grid = vt.snapshot();
        let line = &grid.lines[0].cells;

        assert_eq!(line[0].ch, 'r');
        assert_eq!(line[0].fg, Color::Indexed(1));
        assert!(line[0].attrs & attrs::BOLD != 0);

        assert_eq!(line[4].ch, 'p');
        assert_eq!(line[4].fg, Color::Default);
        assert_eq!(line[4].attrs, 0);

        assert_eq!(line[9].ch, 'r');
        assert_eq!(line[9].fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn bright_and_background_colours_are_distinguished() {
        let mut vt = Vt::new(2, 10);
        vt.feed(b"\x1b[92;44mx");
        let cell = vt.snapshot().lines[0].cells[0];
        assert_eq!(cell.fg, Color::Indexed(10));
        assert_eq!(cell.bg, Color::Indexed(4));
    }

    #[test]
    fn erase_and_home_clear_the_screen_and_move_the_cursor() {
        let mut vt = Vt::new(3, 8);
        vt.feed(b"first\r\nsecond\r\nthird");
        vt.feed(b"\x1b[2J\x1b[H");
        let grid = vt.snapshot();
        for row in 0..3 {
            assert_eq!(text(&grid, row), "", "row {row} survived the erase");
        }
        assert_eq!(
            grid.cursor,
            Cursor {
                row: 0,
                col: 0,
                visible: true
            }
        );
    }

    #[test]
    fn erase_to_end_of_line_leaves_what_came_before_it() {
        let mut vt = Vt::new(2, 10);
        vt.feed(b"abcdefgh\x1b[1;4H\x1b[K");
        assert_eq!(text(&vt.snapshot(), 0), "abc");
    }

    #[test]
    fn text_wraps_at_the_margin_and_stops_wrapping_when_decawm_is_off() {
        let mut vt = Vt::new(3, 4);
        vt.feed(b"abcdef");
        let grid = vt.snapshot();
        assert_eq!(text(&grid, 0), "abcd");
        assert_eq!(text(&grid, 1), "ef");

        let mut no_wrap = Vt::new(3, 4);
        no_wrap.feed(b"\x1b[?7labcdef");
        let grid = no_wrap.snapshot();
        assert_eq!(
            text(&grid, 0),
            "abcf",
            "the last column must be overwritten"
        );
        assert_eq!(text(&grid, 1), "");
    }

    #[test]
    fn multibyte_utf8_becomes_one_cell_per_character() {
        let mut vt = Vt::new(2, 12);
        vt.feed("zażółć".as_bytes());
        let grid = vt.snapshot();
        assert_eq!(text(&grid, 0), "zażółć");
        assert_eq!(grid.lines[0].cells[2].ch, 'ż');

        // Split across two chunks, as a PTY read will do.
        let mut split = Vt::new(2, 12);
        let bytes = "ó".as_bytes();
        split.feed(&bytes[..1]);
        split.feed(&bytes[1..]);
        assert_eq!(text(&split.snapshot(), 0), "ó");
    }

    #[test]
    fn a_double_width_character_takes_two_cells() {
        let mut vt = Vt::new(2, 6);
        vt.feed("漢字".as_bytes());
        let grid = vt.snapshot();
        assert_eq!(grid.lines[0].cells[0].ch, '漢');
        assert_eq!(grid.lines[0].cells[1].ch, '\0');
        assert_eq!(grid.lines[0].cells[2].ch, '字');
        assert_eq!(grid.cursor.col, 4);
    }

    #[test]
    fn tabs_advance_to_the_next_stop() {
        let mut vt = Vt::new(2, 20);
        vt.feed(b"a\tb");
        let grid = vt.snapshot();
        assert_eq!(grid.lines[0].cells[0].ch, 'a');
        assert_eq!(grid.lines[0].cells[8].ch, 'b');
    }

    #[test]
    fn a_scroll_region_scrolls_only_itself() {
        let mut vt = Vt::new(5, 6);
        vt.feed(b"top\r\n\x1b[2;4r\x1b[2;1Ha\r\nb\r\nc\r\nd");
        let grid = vt.snapshot();
        assert_eq!(text(&grid, 0), "top", "the region must not touch row 1");
        assert_eq!(text(&grid, 1), "b");
        assert_eq!(text(&grid, 2), "c");
        assert_eq!(text(&grid, 3), "d");
    }

    #[test]
    fn scrolled_off_lines_go_to_the_scrollback() {
        let mut vt = Vt::new(2, 8);
        vt.feed(b"one\r\ntwo\r\nthree");
        assert_eq!(vt.scrollback().len(), 1);
        let first: String = vt.scrollback()[0]
            .iter()
            .map(|cell| cell.ch)
            .collect::<String>()
            .trim_end()
            .to_string();
        assert_eq!(first, "one");
    }

    #[test]
    fn the_alternate_screen_does_not_disturb_the_scrollback_underneath() {
        let mut vt = Vt::new(3, 8);
        vt.feed(b"shell\r\n");
        vt.feed(b"\x1b[?1049h");
        vt.feed(b"editor\r\n\r\n\r\n\r\n");
        vt.feed(b"\x1b[?1049l");
        assert_eq!(text(&vt.snapshot(), 0), "shell");
        assert!(
            vt.scrollback().is_empty(),
            "the alternate screen pushed lines into the history"
        );
    }

    #[test]
    fn insert_and_delete_move_the_rest_of_the_line() {
        let mut vt = Vt::new(2, 8);
        vt.feed(b"abcdef\x1b[1;1H\x1b[2P");
        assert_eq!(text(&vt.snapshot(), 0), "cdef");

        let mut inserting = Vt::new(2, 8);
        inserting.feed(b"abcdef\x1b[1;1H\x1b[2@");
        assert_eq!(text(&inserting.snapshot(), 0), "  abcdef");
    }

    #[test]
    fn an_operating_system_command_is_consumed_and_never_printed() {
        let mut vt = Vt::new(2, 20);
        vt.feed(b"\x1b]0;a window title\x07done");
        assert_eq!(text(&vt.snapshot(), 0), "done");

        let mut with_st = Vt::new(2, 20);
        with_st.feed(b"\x1b]0;title\x1b\\done");
        assert_eq!(text(&with_st.snapshot(), 0), "done");
    }

    #[test]
    fn the_revision_only_ever_grows_and_changes_are_limited_to_touched_rows() {
        let mut vt = Vt::new(4, 10);
        vt.feed(b"one\r\ntwo\r\nthree");
        let after_first = vt.revision();
        let full = vt.snapshot();
        assert_eq!(full.lines.len(), 4);

        vt.feed(b"\x1b[4;1Hfour");
        assert!(vt.revision() > after_first, "the revision did not advance");

        let delta = vt.changes_since(after_first);
        assert_eq!(delta.lines.len(), 1, "more rows than were touched");
        assert_eq!(delta.lines[0].index, 3);
        assert_eq!(
            delta.lines[0]
                .cells
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim_end(),
            "four"
        );

        // Asking again with the newest revision yields nothing.
        assert!(vt.changes_since(vt.revision()).lines.is_empty());
        // And an empty chunk must not invent a revision.
        let steady = vt.revision();
        vt.feed(b"");
        assert_eq!(vt.revision(), steady);
    }

    #[test]
    fn a_resize_marks_every_row_so_a_client_repaints() {
        let mut vt = Vt::new(4, 10);
        vt.feed(b"hello");
        let before = vt.revision();
        vt.resize(6, 20);
        let delta = vt.changes_since(before);
        assert_eq!(delta.rows, 6);
        assert_eq!(delta.cols, 20);
        assert_eq!(delta.lines.len(), 6);
        assert_eq!(text(&vt.snapshot(), 0), "hello");
    }

    #[test]
    fn the_cursor_can_be_hidden_and_shown() {
        let mut vt = Vt::new(2, 4);
        vt.feed(b"\x1b[?25l");
        assert!(!vt.snapshot().cursor.visible);
        vt.feed(b"\x1b[?25h");
        assert!(vt.snapshot().cursor.visible);
    }

    #[test]
    fn cursor_movement_sequences_land_where_they_say() {
        let mut vt = Vt::new(5, 10);
        vt.feed(b"\x1b[3;5H");
        assert_eq!(
            vt.snapshot().cursor,
            Cursor {
                row: 2,
                col: 4,
                visible: true
            }
        );
        vt.feed(b"\x1b[2A\x1b[3C");
        assert_eq!(
            vt.snapshot().cursor,
            Cursor {
                row: 0,
                col: 7,
                visible: true
            }
        );
        vt.feed(b"\x1b[10B");
        assert_eq!(
            vt.snapshot().cursor.row,
            4,
            "movement must clamp to the screen"
        );
    }

    // --- the registry, which needs a real process -------------------------

    fn registry() -> (tempfile::TempDir, TerminalRegistry) {
        let dir = tempfile::tempdir().unwrap();
        let registry = TerminalRegistry::new(dir.path());
        (dir, registry)
    }

    fn env_at(dir: &Path) -> ExecEnv {
        ExecEnv::new(
            dir.join("home"),
            dir.join("tmp"),
            dir.join("tc/base"),
            dir.join("tc/ov"),
        )
    }

    fn open_spec() -> PtyOpen {
        PtyOpen {
            terminal_id: "t-1".into(),
            session_id: "s-1".into(),
            rows: 10,
            cols: 40,
            shell: Some(vec!["/bin/sh".into()]),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_terminal_echoes_what_the_shell_prints_and_closes_by_reaping_it() {
        let (dir, registry) = registry();
        let target = ExecTarget::Local {
            cwd: dir.path().to_path_buf(),
        };
        let spec = open_spec();
        let opened = registry
            .pty_open(AutonomyMode::Normal, &target, &env_at(dir.path()), &spec)
            .expect("open");

        // The shell start is an exec event, with argv element by element.
        assert_eq!(opened.exec.argv, vec!["/bin/sh"]);
        assert_eq!(opened.exec.session_id, "s-1");
        let record = registry.record_path("s-1", "t-1");
        assert!(record.exists(), "a running terminal left no orphan record");

        let handle = opened.handle.clone();
        registry
            .pty_write(&handle, b"printf 'HELLO-TERMINAL'\n")
            .expect("write");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut seen = false;
        while std::time::Instant::now() < deadline && !seen {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let grid = registry.snapshot(&handle).expect("snapshot");
            seen = grid.lines.iter().any(|line| {
                line.cells
                    .iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .contains("HELLO-TERMINAL")
            });
        }
        assert!(seen, "the shell's output never reached the grid");
        assert!(
            registry.snapshot(&handle).unwrap().revision > 1,
            "the revision never advanced"
        );

        let pid = registry
            .terminals
            .lock()
            .unwrap()
            .get("t-1")
            .map(|t| t.pid)
            .expect("terminal");
        assert_eq!(
            registry.pty_close(&handle).expect("close"),
            TerminalState::Reaped
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while backend::process_alive(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!backend::process_alive(pid), "the shell survived pty_close");
        assert!(!record.exists(), "the orphan record survived the close");
        assert!(matches!(
            registry.snapshot(&handle),
            Err(TerminalError::Unknown(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_terminal_of_another_session_is_not_reachable_by_id() {
        let (dir, registry) = registry();
        let target = ExecTarget::Local {
            cwd: dir.path().to_path_buf(),
        };
        let opened = registry
            .pty_open(
                AutonomyMode::Normal,
                &target,
                &env_at(dir.path()),
                &open_spec(),
            )
            .expect("open");

        let stolen = PtyHandle {
            terminal_id: opened.handle.terminal_id.clone(),
            session_id: "s-2".into(),
        };
        assert!(matches!(
            registry.snapshot(&stolen),
            Err(TerminalError::Unknown(_))
        ));
        assert!(matches!(
            registry.pty_close(&stolen),
            Err(TerminalError::Unknown(_))
        ));
        registry.pty_close(&opened.handle).expect("close");
    }

    #[cfg(unix)]
    #[test]
    fn a_resize_reaches_both_the_grid_and_the_process() {
        let (dir, registry) = registry();
        let target = ExecTarget::Local {
            cwd: dir.path().to_path_buf(),
        };
        let opened = registry
            .pty_open(
                AutonomyMode::Normal,
                &target,
                &env_at(dir.path()),
                &open_spec(),
            )
            .expect("open");
        registry
            .pty_resize(&opened.handle, 30, 100)
            .expect("resize");
        let grid = registry.snapshot(&opened.handle).expect("snapshot");
        assert_eq!((grid.rows, grid.cols), (30, 100));
        registry.pty_close(&opened.handle).expect("close");
    }

    #[cfg(unix)]
    #[test]
    fn an_orphan_from_a_previous_life_is_killed_and_recorded_as_reaped() {
        let (dir, registry) = registry();
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 60"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn");
        let pid = child.id() as i32;

        let record = registry.record_path("s-1", "t-9");
        write_record(
            &record,
            &OrphanRecord {
                terminal_id: "t-9".into(),
                session_id: "s-1".into(),
                pid,
                argv: vec!["/bin/sh".into()],
                started_at: chrono::Utc::now().to_rfc3339(),
                // The record has to identify THIS process, or the reaper
                // refuses to kill it — which is the whole point of the field.
                identity: process_identity(pid),
            },
        )
        .expect("record");

        let reaped = registry.reap_orphans().expect("reap");
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0].terminal_id, "t-9");
        assert_eq!(reaped[0].state, TerminalState::Reaped);
        assert!(!record.exists(), "the record survived the reap");

        let _ = child.wait();
        assert!(!backend::process_alive(pid), "the orphan is still running");
        drop(dir);
    }

    #[test]
    fn plan_mode_has_no_terminal() {
        let (dir, registry) = registry();
        let target = ExecTarget::Local {
            cwd: dir.path().to_path_buf(),
        };
        // The refusal comes before anything is started, so it cannot be worked
        // around by handing in a sandbox that was already materialised.
        let error = registry
            .pty_open(
                AutonomyMode::Plan,
                &target,
                &env_at(dir.path()),
                &open_spec(),
            )
            .expect_err("plan mode must refuse a terminal");
        assert!(matches!(error, TerminalError::NotAvailableInPlanMode));
        assert!(registry.session_handles("s-1").is_empty());
        assert!(
            !registry.record_path("s-1", "t-1").exists(),
            "a refused terminal still left a record"
        );

        // Every other mode may open one.
        for mode in [
            AutonomyMode::Normal,
            AutonomyMode::AutoEdit,
            AutonomyMode::Autonomous,
        ] {
            let result = registry.pty_open(mode, &target, &env_at(dir.path()), &open_spec());
            match result {
                Ok(opened) => {
                    registry.pty_close(&opened.handle).expect("close");
                }
                Err(TerminalError::NotAvailableInPlanMode) => {
                    panic!("{mode:?} was treated as plan mode")
                }
                // A platform without a PTY backend refuses for its own reason,
                // which is exactly what it should say.
                Err(_) => {}
            }
        }
    }

    #[test]
    fn ids_that_could_escape_the_record_directory_are_refused() {
        for bad in ["", "..", "a/b", "A", "x y", &"z".repeat(65)] {
            assert!(validate_id(bad).is_err(), "accepted {bad:?}");
        }
        assert!(validate_id("t-1").is_ok());
    }
}
