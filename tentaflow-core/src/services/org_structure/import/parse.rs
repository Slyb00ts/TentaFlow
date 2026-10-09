//! Bytes of a CSV or XLSX file into rows of text. Nothing here knows what a
//! column means: that is `columns` and `rows`.

use std::cell::Cell;
use std::io::{BufReader, Cursor, Read};
use std::rc::Rc;

use calamine::{Data, Reader, Xlsx};

use super::report::FileError;
use super::{
    FileFormat, MAX_EXPANDED_XLSX_BYTES, MAX_FILE_BYTES, MAX_ROWS, MAX_SHEET_COLS, MAX_SHEET_ROWS,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRow {
    /// The row number the spreadsheet shows (the header row is the first one
    /// with content), so a report points at what the user sees.
    pub number: u32,
    pub cells: Vec<String>,
}

#[derive(Debug)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<RawRow>,
    /// The worksheet the rows come from (XLSX only).
    pub sheet: Option<String>,
    /// Other worksheets that hold data; the import reads one sheet only.
    pub other_sheets: Vec<String>,
}

pub fn read(format: FileFormat, bytes: &[u8]) -> Result<Table, FileError> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(FileError::TooLarge {
            bytes: bytes.len(),
            max: MAX_FILE_BYTES,
        });
    }
    if bytes.is_empty() {
        return Err(FileError::Empty);
    }
    match format {
        FileFormat::Csv => read_csv(bytes),
        FileFormat::Xlsx => read_xlsx(bytes),
    }
}

/// The characters that make a spreadsheet run a cell as a formula, the
/// full-width forms (`＝ ＋ － ＠`) some spreadsheets fold into the ASCII ones included.
pub const FORMULA_STARTS: [char; 10] = [
    '=', '+', '-', '@', '\t', '\r', '\u{ff1d}', '\u{ff0b}', '\u{ff0d}', '\u{ff20}',
];

/// What a spreadsheet skips before it decides whether a cell is a formula. A tab
/// or a carriage return is not padding: it is a start of its own.
fn is_padding(c: char) -> bool {
    (c.is_whitespace() && !matches!(c, '\t' | '\r' | '\n')) || matches!(c, '\u{200b}' | '\u{feff}')
}

/// True when a spreadsheet could read the cell as a formula: it starts with one
/// of `FORMULA_STARTS`, after any quotes and padding the guard may have put or
/// the user typed in front.
pub fn leads_into_formula(text: &str) -> bool {
    text.trim_start_matches(|c: char| c == '\'' || is_padding(c))
        .starts_with(FORMULA_STARTS)
}

/// Spreadsheets pad cells with spaces, non-breaking spaces and zero-width
/// marks; the export puts one `'` in front of a cell that leads into a
/// formula, or like a quote and then a formula, which is undone here. The two
/// rules are inverse of each other, so a name that really starts with `'=`
/// comes back as typed.
pub fn clean_cell(raw: &str) -> String {
    let trimmed =
        raw.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{200b}' | '\u{feff}'));
    if trimmed.starts_with('\'') && leads_into_formula(trimmed) {
        return trimmed[1..].to_string();
    }
    trimmed.to_string()
}

/// Splits the first row with content off as the header and collects the
/// rows with content after it.
struct Collector {
    headers: Option<Vec<String>>,
    rows: Vec<RawRow>,
}

impl Collector {
    fn new() -> Self {
        Self {
            headers: None,
            rows: Vec::new(),
        }
    }

    fn push(&mut self, number: u32, cells: Vec<String>) -> Result<(), FileError> {
        if cells.iter().all(String::is_empty) {
            return Ok(());
        }
        if self.headers.is_none() {
            self.headers = Some(cells);
            return Ok(());
        }
        if self.rows.len() >= MAX_ROWS {
            return Err(FileError::TooManyRows { max: MAX_ROWS });
        }
        self.rows.push(RawRow { number, cells });
        Ok(())
    }

    fn finish(self) -> Result<Table, FileError> {
        Ok(Table {
            headers: self.headers.ok_or(FileError::Empty)?,
            rows: self.rows,
            sheet: None,
            other_sheets: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------------

/// Bytes Windows-1250 leaves undefined.
const CP1250_UNDEFINED: [u8; 5] = [0x81, 0x83, 0x88, 0x90, 0x98];

/// Whether the bytes hold a well-formed UTF-8 sequence of two or more bytes.
fn has_utf8_multibyte(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        let len = match bytes[i] {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => 1,
        };
        if len > 1
            && bytes
                .get(i..i + len)
                .is_some_and(|s| std::str::from_utf8(s).is_ok())
        {
            return true;
        }
        i += 1;
    }
    false
}

/// A BOM names its encoding. Without one the file is UTF-8 when it is valid
/// UTF-8. Excel's "CSV" of a Polish machine is Windows-1250, so bytes that are
/// not valid UTF-8 are read as Windows-1250 — but only when that reading is
/// safe: no byte Windows-1250 leaves undefined, and no well-formed UTF-8
/// multibyte sequence anywhere (a file that mixes both encodings would be
/// decoded into wrong letters, silently). Anything else is a typed error.
fn decode_text(bytes: &[u8]) -> Result<String, FileError> {
    let invalid = |detail: String| FileError::InvalidEncoding { detail };
    if let Some((encoding, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        return encoding
            .decode_without_bom_handling_and_without_replacement(&bytes[bom_len..])
            .map(|text| text.into_owned())
            .ok_or_else(|| invalid(format!("invalid {} text", encoding.name())));
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_string());
    }
    if bytes.iter().any(|b| CP1250_UNDEFINED.contains(b)) {
        return Err(invalid(
            "not valid UTF-8, and it has bytes Windows-1250 does not define".to_string(),
        ));
    }
    if has_utf8_multibyte(bytes) {
        return Err(invalid(
            "a mix of UTF-8 and another encoding; save the file as UTF-8".to_string(),
        ));
    }
    Ok(encoding_rs::WINDOWS_1250
        .decode_without_bom_handling(bytes)
        .0
        .into_owned())
}

/// The delimiter the header line uses most: Polish Excel writes `;`, others `,`
/// or a tab. Separators inside quotes do not count.
fn sniff_delimiter(text: &str) -> u8 {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let (mut semicolons, mut commas, mut tabs) = (0usize, 0usize, 0usize);
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => semicolons += 1,
            ',' if !quoted => commas += 1,
            '\t' if !quoted => tabs += 1,
            _ => {}
        }
    }
    if semicolons >= commas && semicolons >= tabs && semicolons > 0 {
        b';'
    } else if tabs > commas {
        b'\t'
    } else {
        b','
    }
}

fn read_csv(bytes: &[u8]) -> Result<Table, FileError> {
    let text = decode_text(bytes)?;
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(sniff_delimiter(&text))
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut collector = Collector::new();
    // A spreadsheet numbers the rows of the file, and a quoted cell that spans
    // lines is still one row: the number is the physical line the record
    // starts on less the lines earlier records spent inside their cells (a
    // blank line is a row in a spreadsheet, so it counts). The lines are
    // counted from the text itself, not taken from the csv reader.
    let (mut inner_lines, mut line, mut counted_to) = (0u32, 1u32, 0usize);
    for record in reader.records() {
        let record = record.map_err(|e| FileError::Unreadable {
            format: "csv",
            detail: e.to_string(),
        })?;
        // The reader reports where it began looking, which is before the blank
        // lines it skipped; the record starts after them.
        let mut start = record.position().map_or(counted_to, |p| p.byte() as usize);
        while matches!(text.as_bytes().get(start), Some(b'\n' | b'\r')) {
            start += 1;
        }
        line += text.as_bytes()[counted_to.min(start)..start]
            .iter()
            .filter(|b| **b == b'\n')
            .count() as u32;
        counted_to = start;
        collector.push(
            line.saturating_sub(inner_lines),
            record.iter().map(clean_cell).collect(),
        )?;
        inner_lines += record
            .iter()
            .map(|field| field.matches('\n').count() as u32)
            .sum::<u32>();
    }
    collector.finish()
}

// ---------------------------------------------------------------------------
// XLSX
// ---------------------------------------------------------------------------

/// Bytes inflated so far, shared by every entry of one workbook.
struct Budget {
    remaining: Cell<u64>,
    exceeded: Cell<bool>,
}

struct Limited<R> {
    inner: R,
    budget: Rc<Budget>,
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n as u64 > self.budget.remaining.get() {
            self.budget.exceeded.set(true);
            return Err(std::io::Error::other("the workbook inflates too far"));
        }
        self.budget
            .remaining
            .set(self.budget.remaining.get() - n as u64);
        Ok(n)
    }
}

/// The part of `A12` before the digits as a 1-based column number.
fn column_of(reference: &[u8]) -> u32 {
    reference
        .iter()
        .take_while(|b| b.is_ascii_alphabetic())
        .fold(0u32, |acc, b| {
            acc.saturating_mul(26)
                .saturating_add((b.to_ascii_uppercase() - b'A' + 1) as u32)
        })
}

fn row_of(reference: &[u8]) -> u32 {
    reference
        .iter()
        .skip_while(|b| b.is_ascii_alphabetic())
        .take_while(|b| b.is_ascii_digit())
        .fold(0u32, |acc, b| {
            acc.saturating_mul(10).saturating_add((b - b'0') as u32)
        })
}

/// Walks a worksheet's XML without building anything and refuses a sheet that
/// reaches beyond the caps. calamine allocates a dense range from the first to
/// the last cell, so one cell at XFD1048576 would cost gigabytes.
fn ensure_sheet_bounded(source: impl Read) -> Result<(), FileError> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(BufReader::new(source));
    let mut buffer = Vec::new();
    let (mut rows, mut row_number, mut column): (u32, u32, u32) = (0, 0, 0);
    let too_large = |rows: u32, cols: u32| FileError::SheetTooLarge {
        rows,
        cols,
        max_rows: MAX_SHEET_ROWS as u32,
        max_cols: MAX_SHEET_COLS as u32,
    };
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|e| FileError::Unreadable {
                format: "xlsx",
                detail: e.to_string(),
            })?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => match e.local_name().as_ref() {
                b"row" => {
                    rows += 1;
                    row_number += 1;
                    column = 0;
                    for attribute in e.attributes().flatten() {
                        if attribute.key.as_ref() == b"r" {
                            row_number = row_of(attribute.value.as_ref()).max(row_number);
                        }
                    }
                }
                b"c" => {
                    column += 1;
                    for attribute in e.attributes().flatten() {
                        if attribute.key.as_ref() == b"r" {
                            let reference = attribute.value.as_ref();
                            column = column_of(reference).max(column);
                            row_number = row_of(reference).max(row_number);
                        }
                    }
                }
                _ => {}
            },
            Event::Eof => return Ok(()),
            _ => {}
        }
        if rows as usize > MAX_SHEET_ROWS
            || row_number as usize > MAX_SHEET_ROWS
            || column as usize > MAX_SHEET_COLS
        {
            return Err(too_large(row_number.max(rows), column));
        }
        buffer.clear();
    }
}

/// Inflates every entry of the workbook once, counting the bytes that really
/// come out — the sizes the archive declares are not trusted — and checks
/// every worksheet's extent, before calamine allocates anything.
fn ensure_workbook_bounded(bytes: &[u8]) -> Result<(), FileError> {
    let unreadable = |detail: String| FileError::Unreadable {
        format: "xlsx",
        detail,
    };
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| unreadable(e.to_string()))?;
    let budget = Rc::new(Budget {
        remaining: Cell::new(MAX_EXPANDED_XLSX_BYTES),
        exceeded: Cell::new(false),
    });
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| unreadable(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let is_sheet = entry.name().starts_with("xl/worksheets/") && entry.name().ends_with(".xml");
        let limited = Limited {
            inner: entry,
            budget: budget.clone(),
        };
        let outcome = if is_sheet {
            ensure_sheet_bounded(limited)
        } else {
            let mut limited = limited;
            std::io::copy(&mut limited, &mut std::io::sink())
                .map(|_| ())
                .map_err(|e| unreadable(e.to_string()))
        };
        if budget.exceeded.get() {
            return Err(FileError::ExpandsTooLarge {
                max: MAX_EXPANDED_XLSX_BYTES,
            });
        }
        outcome?;
    }
    Ok(())
}

/// A number as a person would type it: `12`, not `12.0`.
fn number_text(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn xlsx_cell(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(text) => clean_cell(text),
        Data::Float(value) => number_text(*value),
        Data::Int(value) => value.to_string(),
        Data::Bool(value) => value.to_string(),
        // A date is written as an ISO day, so it needs no guessing of the
        // spreadsheet's date system or the locale of the machine that made it.
        Data::DateTime(value) if value.is_datetime() => {
            let (year, month, day, ..) = value.to_ymd_hms_milli();
            format!("{year:04}-{month:02}-{day:02}")
        }
        Data::DateTime(value) => number_text(value.as_f64()),
        Data::DateTimeIso(text) | Data::DurationIso(text) => clean_cell(text),
        Data::Error(_) => String::new(),
    }
}

fn read_xlsx(bytes: &[u8]) -> Result<Table, FileError> {
    ensure_workbook_bounded(bytes)?;
    let unreadable = |detail: String| FileError::Unreadable {
        format: "xlsx",
        detail,
    };
    let mut workbook: Xlsx<_> =
        Xlsx::new(Cursor::new(bytes)).map_err(|e| unreadable(e.to_string()))?;
    let mut table: Option<Table> = None;
    let mut other_sheets = Vec::new();
    for name in workbook.sheet_names() {
        let range = workbook
            .worksheet_range(&name)
            .map_err(|e| unreadable(e.to_string()))?;
        let Some((first_row, _)) = range.start() else {
            continue;
        };
        if table.is_some() {
            other_sheets.push(name);
            continue;
        }
        let mut collector = Collector::new();
        for (offset, cells) in range.rows().enumerate() {
            collector.push(
                first_row + offset as u32 + 1,
                cells.iter().map(xlsx_cell).collect(),
            )?;
        }
        match collector.finish() {
            Ok(mut read) => {
                read.sheet = Some(name);
                table = Some(read);
            }
            Err(FileError::Empty) => {}
            Err(other) => return Err(other),
        }
    }
    let mut table = table.ok_or(FileError::Empty)?;
    table.other_sheets = other_sheets;
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv(text: &str) -> Table {
        read(FileFormat::Csv, text.as_bytes()).unwrap()
    }

    #[test]
    fn the_delimiter_is_taken_from_the_header_line() {
        for (delimiter, text) in [
            (';', "a;b;c\n1;2;3\n"),
            (',', "a,b,c\n1,2,3\n"),
            ('\t', "a\tb\tc\n1\t2\t3\n"),
        ] {
            let table = csv(text);
            assert_eq!(table.headers, ["a", "b", "c"], "{delimiter:?}");
            assert_eq!(table.rows[0].cells, ["1", "2", "3"], "{delimiter:?}");
        }
    }

    #[test]
    fn a_separator_inside_quotes_is_data() {
        let table = csv("name;note\n\"Kowalski, Jan\";\"a;b\"\n");
        assert_eq!(table.rows[0].cells, ["Kowalski, Jan", "a;b"]);
    }

    #[test]
    fn a_bom_and_padding_are_dropped() {
        let bytes = b"\xef\xbb\xbfkod jednostki;nazwa jednostki\n  IT \xc2\xa0;\xe2\x80\x8bDzia\xc5\x82 IT\n";
        let table = read(FileFormat::Csv, bytes).unwrap();
        assert_eq!(table.headers, ["kod jednostki", "nazwa jednostki"]);
        assert_eq!(table.rows[0].cells, ["IT", "Dział IT"]);
    }

    #[test]
    fn excel_ansi_files_are_read_as_windows_1250() {
        // "Dział Ślązaków" in Windows-1250 is not valid UTF-8.
        let (bytes, _, _) = encoding_rs::WINDOWS_1250.encode("kod;nazwa\nA;Dział Ślązaków\n");
        let table = read(FileFormat::Csv, &bytes).unwrap();
        assert_eq!(table.rows[0].cells[1], "Dział Ślązaków");
    }

    #[test]
    fn utf16_with_a_bom_is_read() {
        let mut bytes = vec![0xff, 0xfe];
        for unit in "kod\tnazwa\nA\tŁódź\n".encode_utf16() {
            bytes.extend(unit.to_le_bytes());
        }
        let table = read(FileFormat::Csv, &bytes).unwrap();
        assert_eq!(table.headers, ["kod", "nazwa"]);
        assert_eq!(table.rows[0].cells, ["A", "Łódź"]);
    }

    #[test]
    fn row_numbers_are_the_lines_the_spreadsheet_shows_and_blank_rows_are_skipped() {
        let table = csv("a;b\n1;2\n;\n3;4\n");
        let numbers: Vec<u32> = table.rows.iter().map(|r| r.number).collect();
        assert_eq!(numbers, [2, 4]);
    }

    #[test]
    fn a_formula_guard_is_undone_but_a_plain_quote_is_kept() {
        assert_eq!(clean_cell("'=SUM(A1)"), "=SUM(A1)");
        assert_eq!(clean_cell("'-Kowalski"), "-Kowalski");
        assert_eq!(clean_cell("'Kowalski"), "'Kowalski");
        // The export puts one quote in front; one is taken off.
        assert_eq!(clean_cell("''=x"), "'=x");
        assert_eq!(clean_cell("'=x"), "=x");
    }

    #[test]
    fn padding_and_full_width_signs_lead_into_a_formula_too() {
        for text in [
            "= 1",
            " =1",
            "\u{a0}+1",
            "\u{200b}@x",
            "\u{ff1d}1",
            "\u{ff0b}1",
            "\u{ff0d}1",
            "\u{ff20}x",
            "\t=1",
            "' =1",
        ] {
            assert!(leads_into_formula(text), "{text:?}");
        }
        for text in ["Kowalski", "'Kowalski", " Kowalski", "a=b", ""] {
            assert!(!leads_into_formula(text), "{text:?}");
        }
        assert_eq!(clean_cell("' =x"), " =x");
        assert_eq!(clean_cell("'\u{ff1d}x"), "\u{ff1d}x");
    }

    #[test]
    fn oversized_and_empty_files_are_typed_errors() {
        let big = vec![b'a'; MAX_FILE_BYTES + 1];
        assert!(matches!(
            read(FileFormat::Csv, &big),
            Err(FileError::TooLarge { .. })
        ));
        assert_eq!(read(FileFormat::Csv, b"").unwrap_err(), FileError::Empty);
        assert_eq!(
            read(FileFormat::Csv, b"\n;\n").unwrap_err(),
            FileError::Empty
        );
    }

    #[test]
    fn more_rows_than_the_limit_is_a_typed_error() {
        let mut text = String::from("a;b\n");
        for _ in 0..=MAX_ROWS {
            text.push_str("1;2\n");
        }
        assert_eq!(
            read(FileFormat::Csv, text.as_bytes()).unwrap_err(),
            FileError::TooManyRows { max: MAX_ROWS }
        );
    }

    #[test]
    fn something_that_is_not_a_workbook_is_unreadable() {
        assert!(matches!(
            read(FileFormat::Xlsx, b"not a zip at all"),
            Err(FileError::Unreadable { format: "xlsx", .. })
        ));
    }
}
