// ===== File: ml_studio/profile.rs — tabular dataset profiler (CSV/XLSX) =====
//
// Parses an uploaded tabular file (CSV or XLSX) and profiles every column:
// detected type, unique count, missing ratio, sample values and — for small
// categorical columns — the class histogram that feeds the "wykryto N klas"
// UI hint. Pure CPU work, no GPU. All failures surface as `anyhow::Error`
// (never a panic), and hard limits bound memory regardless of input size.

use std::collections::BTreeMap;
use std::collections::HashSet;

use anyhow::{bail, Result};
use calamine::{Data, Reader, Xlsx};
use serde::{Deserialize, Serialize};

/// Upper bound on rows scanned for profiling. Rows beyond this are ignored for
/// statistics but still counted into `row_count` (so the UI shows the true size).
const MAX_PROFILE_ROWS: usize = 100_000;

/// Upper bound on accepted upload size. A larger payload is rejected before any
/// parsing so a hostile or accidental huge file cannot exhaust memory.
///
/// Inline upload rides the binary WebSocket transport, whose single-frame limit
/// is 1 MiB (`api::dashboard::ws_binary::MAX_FRAME_SIZE`). The CBOR `Envelope`
/// adds a small header/routing overhead on top of the raw `bytes`, so the
/// effective payload must stay under 1 MiB; `1_000_000` leaves headroom for the
/// envelope. Larger datasets need a chunked / dedicated upload endpoint
/// (future work B1.2). A bigger file is rejected here with a clear error before
/// any parsing.
const MAX_BYTES: usize = 1_000_000;

/// Upper bound on columns. Wider tables are rejected rather than profiled, to
/// keep per-column accumulators bounded.
const MAX_COLUMNS: usize = 4096;

/// A categorical column with at most this many distinct values gets a full class
/// histogram (`ClassCount` list). Above it, only the unique count is reported.
const CATEGORICAL_MAX_CLASSES: usize = 50;

/// Sample values retained per column for the UI preview.
const SAMPLE_LIMIT: usize = 5;

/// How many non-empty values per column are inspected for type inference.
const TYPE_SAMPLE_LIMIT: usize = 1000;

/// Detected logical type of a column, inferred from a sample of its values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnType {
    /// Small distinct set relative to row count — treated as a category.
    Categorical,
    /// All sampled non-empty values parse as integers.
    Integer,
    /// All sampled non-empty values parse as floats (and not all integers).
    Float,
    /// All sampled non-empty values parse as a date/datetime.
    Date,
    /// Free-form text (fallback).
    Text,
}

impl ColumnType {
    /// Stable machine slug carried on the wire and shown (localised) in the UI.
    pub fn slug(self) -> &'static str {
        match self {
            ColumnType::Categorical => "categorical",
            ColumnType::Integer => "integer",
            ColumnType::Float => "float",
            ColumnType::Date => "date",
            ColumnType::Text => "text",
        }
    }
}

/// One distinct value of a categorical column and how many rows carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassCount {
    pub value: String,
    pub count: u64,
}

/// Profile of a single column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnProfile {
    pub name: String,
    pub column_type: ColumnType,
    /// Distinct non-empty values seen (capped accounting below the cardinality
    /// cap; `unique_capped` marks when counting stopped being exact).
    pub unique_count: u64,
    /// Fraction of scanned rows whose value is missing/empty, in `[0.0, 1.0]`.
    pub missing_ratio: f64,
    /// Up to `SAMPLE_LIMIT` example non-empty values.
    pub examples: Vec<String>,
    /// Class histogram for categorical columns with `<= CATEGORICAL_MAX_CLASSES`
    /// distinct values; empty otherwise.
    pub classes: Vec<ClassCount>,
    /// True when the distinct-value tracker hit its cap and `unique_count` is a
    /// lower bound rather than an exact count.
    pub unique_capped: bool,
}

/// Full profile of an uploaded table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableProfile {
    /// Detected source format: `"csv"` or `"xlsx"`.
    pub format: String,
    /// Total data rows present in the file (excludes the header row). May exceed
    /// the number of rows actually scanned for statistics (see `scanned_rows`).
    pub row_count: u64,
    /// Rows actually scanned for per-column statistics (`<= MAX_PROFILE_ROWS`).
    pub scanned_rows: u64,
    pub column_count: u32,
    pub columns: Vec<ColumnProfile>,
    /// True when the file had more than `MAX_PROFILE_ROWS` rows and statistics
    /// were computed on a prefix only.
    pub truncated: bool,
}

/// Per-column accumulator used while streaming rows.
struct ColumnAcc {
    name: String,
    non_empty: u64,
    missing: u64,
    /// Distinct values, tracked until the cardinality cap is reached.
    distinct: HashSet<String>,
    distinct_capped: bool,
    /// Value -> count, kept only while distinct cardinality stays small enough
    /// to be worth a histogram.
    histogram: BTreeMap<String, u64>,
    examples: Vec<String>,
    /// Non-empty samples retained for type inference.
    type_sample: Vec<String>,
}

/// Cardinality cap for the distinct-value tracker. Beyond this a column is
/// certainly not categorical, so exact counting is abandoned to bound memory.
const DISTINCT_CAP: usize = 10_000;

impl ColumnAcc {
    fn new(name: String) -> Self {
        Self {
            name,
            non_empty: 0,
            missing: 0,
            distinct: HashSet::new(),
            distinct_capped: false,
            histogram: BTreeMap::new(),
            examples: Vec::new(),
            type_sample: Vec::new(),
        }
    }

    fn observe(&mut self, raw: &str) {
        let value = raw.trim();
        if value.is_empty() {
            self.missing += 1;
            return;
        }
        self.non_empty += 1;

        if !self.distinct_capped {
            if self.distinct.len() < DISTINCT_CAP {
                self.distinct.insert(value.to_string());
            } else if !self.distinct.contains(value) {
                self.distinct_capped = true;
                self.histogram.clear();
            }
        }
        if !self.distinct_capped && self.histogram.len() <= CATEGORICAL_MAX_CLASSES {
            *self.histogram.entry(value.to_string()).or_insert(0) += 1;
        }
        if self.examples.len() < SAMPLE_LIMIT && !self.examples.iter().any(|e| e == value) {
            self.examples.push(value.to_string());
        }
        if self.type_sample.len() < TYPE_SAMPLE_LIMIT {
            self.type_sample.push(value.to_string());
        }
    }

    fn finish(self, scanned_rows: u64) -> ColumnProfile {
        let unique_count = self.distinct.len() as u64;
        let detected = infer_type(
            &self.type_sample,
            unique_count,
            scanned_rows,
            self.distinct_capped,
        );
        let missing_ratio = if scanned_rows == 0 {
            0.0
        } else {
            self.missing as f64 / scanned_rows as f64
        };
        let classes = if detected == ColumnType::Categorical
            && !self.distinct_capped
            && self.histogram.len() <= CATEGORICAL_MAX_CLASSES
        {
            let mut v: Vec<ClassCount> = self
                .histogram
                .into_iter()
                .map(|(value, count)| ClassCount { value, count })
                .collect();
            v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
            v
        } else {
            Vec::new()
        };
        ColumnProfile {
            name: self.name,
            column_type: detected,
            unique_count,
            missing_ratio,
            examples: self.examples,
            classes,
            unique_capped: self.distinct_capped,
        }
    }
}

/// Infers a column type from a sample of its non-empty values plus cardinality
/// context. Order matters: integer before float, then date, then categorical
/// (low cardinality relative to row count), else text.
fn infer_type(
    sample: &[String],
    unique_count: u64,
    scanned_rows: u64,
    distinct_capped: bool,
) -> ColumnType {
    if sample.is_empty() {
        return ColumnType::Text;
    }
    let all_int = sample.iter().all(|v| parse_int(v));
    if all_int {
        // An all-integer column with very few distinct values reads better as a
        // category (e.g. a 0/1 label) than as a numeric measure.
        if is_low_cardinality(unique_count, scanned_rows, distinct_capped) {
            return ColumnType::Categorical;
        }
        return ColumnType::Integer;
    }
    if sample.iter().all(|v| parse_float(v)) {
        return ColumnType::Float;
    }
    if sample.iter().all(|v| parse_date(v)) {
        return ColumnType::Date;
    }
    if is_low_cardinality(unique_count, scanned_rows, distinct_capped) {
        return ColumnType::Categorical;
    }
    ColumnType::Text
}

/// A column is "low cardinality" (categorical) when its distinct set is small in
/// absolute terms (<= `CATEGORICAL_MAX_CLASSES`) AND shows repetition, i.e. it is
/// not a near-unique key or free-text column. A capped distinct tracker means
/// cardinality is high, so never categorical. The absolute cap (not a row
/// fraction) is the deciding signal — a row-fraction rule wrongly demotes a
/// genuine small category on a small sample (e.g. 3 cities across 4 rows).
fn is_low_cardinality(unique_count: u64, scanned_rows: u64, distinct_capped: bool) -> bool {
    if distinct_capped {
        return false;
    }
    if unique_count == 0 {
        return false;
    }
    if unique_count > CATEGORICAL_MAX_CLASSES as u64 {
        return false;
    }
    if scanned_rows == 0 {
        return true;
    }
    // Require at least one repeated value: an all-distinct column reads as an
    // identifier / free text, not a category.
    unique_count < scanned_rows
}

fn parse_int(v: &str) -> bool {
    v.parse::<i64>().is_ok()
}

fn parse_float(v: &str) -> bool {
    matches!(v.parse::<f64>(), Ok(f) if f.is_finite())
}

/// Heuristic date detector: ISO-like `YYYY-MM-DD` optionally followed by a time,
/// or `DD/MM/YYYY` / `MM/DD/YYYY` separated by `/` or `.`. Conservative — only
/// digit/separator shapes with plausible field ranges qualify.
fn parse_date(v: &str) -> bool {
    let date_part = v.split(['T', ' ']).next().unwrap_or(v);
    let seps: &[char] = &['-', '/', '.'];
    let parts: Vec<&str> = date_part.split(seps).collect();
    if parts.len() != 3 {
        return false;
    }
    let nums: Option<Vec<u32>> = parts.iter().map(|p| p.parse::<u32>().ok()).collect();
    let Some(nums) = nums else {
        return false;
    };
    if parts.iter().any(|p| p.is_empty()) {
        return false;
    }
    // Year-first or year-last; require one four-or-fewer-digit year and the other
    // two fields in month/day ranges.
    let plausible =
        |y: u32, m: u32, d: u32| y >= 1 && (1..=12).contains(&m) && (1..=31).contains(&d);
    let (a, b, c) = (nums[0], nums[1], nums[2]);
    plausible(a, b, c) || plausible(c, b, a) || plausible(c, a, b)
}

/// Parses + profiles an uploaded tabular file. `filename` selects the parser by
/// extension (`.csv`/`.tsv` → csv crate, `.xlsx`/`.xlsm` → calamine first sheet).
pub fn profile_table(bytes: &[u8], filename: &str) -> Result<TableProfile> {
    if bytes.is_empty() {
        bail!("uploaded file is empty");
    }
    if bytes.len() > MAX_BYTES {
        bail!(
            "uploaded file is too large ({} bytes, limit {} bytes)",
            bytes.len(),
            MAX_BYTES
        );
    }
    let ext = filename
        .rsplit('.')
        .next()
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "csv" => profile_csv(bytes, b','),
        "tsv" => profile_csv(bytes, b'\t'),
        "xlsx" | "xlsm" | "xlsb" | "xls" => profile_xlsx(bytes),
        other => bail!(
            "unsupported file extension '.{}' (expected csv or xlsx)",
            other
        ),
    }
}

/// Upper bound on data rows materialized by `parse_table`. Training does not
/// stream, so the full row matrix is held in memory; this cap keeps a huge file
/// from exhausting memory even though the upload limit already bounds bytes.
const MAX_PARSE_ROWS: usize = 200_000;

/// Parses an uploaded tabular file into its header names and a dense row matrix
/// of trimmed string cells. Unlike `profile_table` (which streams per-column
/// statistics), this materializes the whole table for downstream training. Each
/// row is padded/truncated to the header width so `rows[i][j]` is always valid.
/// Parser selection mirrors `profile_table` (extension-based).
pub fn parse_table(bytes: &[u8], filename: &str) -> Result<(Vec<String>, Vec<Vec<String>>)> {
    if bytes.is_empty() {
        bail!("uploaded file is empty");
    }
    if bytes.len() > MAX_BYTES {
        bail!(
            "uploaded file is too large ({} bytes, limit {} bytes)",
            bytes.len(),
            MAX_BYTES
        );
    }
    let ext = filename
        .rsplit('.')
        .next()
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "csv" => parse_csv(bytes, b','),
        "tsv" => parse_csv(bytes, b'\t'),
        "xlsx" | "xlsm" | "xlsb" | "xls" => parse_xlsx(bytes),
        other => bail!(
            "unsupported file extension '.{}' (expected csv or xlsx)",
            other
        ),
    }
}

fn parse_csv(bytes: &[u8], delimiter: u8) -> Result<(Vec<String>, Vec<Vec<String>>)> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .has_headers(true)
        .from_reader(bytes);
    let headers = reader
        .headers()
        .map_err(|e| anyhow::anyhow!("failed to read CSV header: {e}"))?
        .clone();
    let names: Vec<String> = headers.iter().map(|h| h.trim().to_string()).collect();
    if names.is_empty() {
        bail!("CSV has no columns");
    }
    if names.len() > MAX_COLUMNS {
        bail!("too many columns ({}, limit {})", names.len(), MAX_COLUMNS);
    }
    let width = names.len();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut record = csv::StringRecord::new();
    while reader
        .read_record(&mut record)
        .map_err(|e| anyhow::anyhow!("failed to read CSV row {}: {e}", rows.len() + 1))?
    {
        if rows.len() >= MAX_PARSE_ROWS {
            break;
        }
        let row: Vec<String> = (0..width)
            .map(|i| record.get(i).unwrap_or("").trim().to_string())
            .collect();
        rows.push(row);
    }
    Ok((names, rows))
}

fn parse_xlsx(bytes: &[u8]) -> Result<(Vec<String>, Vec<Vec<String>>)> {
    let cursor = std::io::Cursor::new(bytes);
    let mut workbook: Xlsx<_> =
        Xlsx::new(cursor).map_err(|e| anyhow::anyhow!("failed to open XLSX: {e}"))?;
    let sheet_name = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("XLSX has no worksheets"))?;
    let range = workbook
        .worksheet_range(&sheet_name)
        .map_err(|e| anyhow::anyhow!("failed to read worksheet '{sheet_name}': {e}"))?;
    let mut iter = range.rows();
    let header_row = iter
        .next()
        .ok_or_else(|| anyhow::anyhow!("XLSX sheet is empty"))?;
    let names: Vec<String> = header_row.iter().map(cell_to_string).collect();
    if names.is_empty() {
        bail!("XLSX sheet has no columns");
    }
    if names.len() > MAX_COLUMNS {
        bail!("too many columns ({}, limit {})", names.len(), MAX_COLUMNS);
    }
    let width = names.len();
    let mut rows: Vec<Vec<String>> = Vec::new();
    for row in iter {
        if rows.len() >= MAX_PARSE_ROWS {
            break;
        }
        let parsed: Vec<String> = (0..width)
            .map(|i| row.get(i).map(cell_to_string).unwrap_or_default())
            .collect();
        rows.push(parsed);
    }
    Ok((names, rows))
}

fn profile_csv(bytes: &[u8], delimiter: u8) -> Result<TableProfile> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .has_headers(true)
        .from_reader(bytes);

    let headers = reader
        .headers()
        .map_err(|e| anyhow::anyhow!("failed to read CSV header: {e}"))?
        .clone();
    let names: Vec<String> = headers.iter().map(|h| h.trim().to_string()).collect();
    if names.is_empty() {
        bail!("CSV has no columns");
    }
    if names.len() > MAX_COLUMNS {
        bail!("too many columns ({}, limit {})", names.len(), MAX_COLUMNS);
    }

    let mut accs: Vec<ColumnAcc> = names.iter().cloned().map(ColumnAcc::new).collect();
    let mut row_count: u64 = 0;
    let mut scanned: u64 = 0;
    let mut record = csv::StringRecord::new();
    loop {
        let has = reader
            .read_record(&mut record)
            .map_err(|e| anyhow::anyhow!("failed to read CSV row {}: {e}", row_count + 1))?;
        if !has {
            break;
        }
        row_count += 1;
        if (scanned as usize) < MAX_PROFILE_ROWS {
            for (i, acc) in accs.iter_mut().enumerate() {
                acc.observe(record.get(i).unwrap_or(""));
            }
            scanned += 1;
        }
    }

    Ok(finish_profile("csv", accs, row_count, scanned))
}

fn profile_xlsx(bytes: &[u8]) -> Result<TableProfile> {
    let cursor = std::io::Cursor::new(bytes);
    let mut workbook: Xlsx<_> =
        Xlsx::new(cursor).map_err(|e| anyhow::anyhow!("failed to open XLSX: {e}"))?;
    let sheet_name = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("XLSX has no worksheets"))?;
    let range = workbook
        .worksheet_range(&sheet_name)
        .map_err(|e| anyhow::anyhow!("failed to read worksheet '{sheet_name}': {e}"))?;

    let mut rows = range.rows();
    let header_row = rows
        .next()
        .ok_or_else(|| anyhow::anyhow!("XLSX sheet is empty"))?;
    let names: Vec<String> = header_row.iter().map(cell_to_string).collect();
    if names.is_empty() {
        bail!("XLSX sheet has no columns");
    }
    if names.len() > MAX_COLUMNS {
        bail!("too many columns ({}, limit {})", names.len(), MAX_COLUMNS);
    }

    let mut accs: Vec<ColumnAcc> = names.iter().cloned().map(ColumnAcc::new).collect();
    let mut row_count: u64 = 0;
    let mut scanned: u64 = 0;
    for row in rows {
        row_count += 1;
        if (scanned as usize) < MAX_PROFILE_ROWS {
            for (i, acc) in accs.iter_mut().enumerate() {
                let cell = row.get(i).map(cell_to_string).unwrap_or_default();
                acc.observe(&cell);
            }
            scanned += 1;
        }
    }

    Ok(finish_profile("xlsx", accs, row_count, scanned))
}

fn finish_profile(
    format: &str,
    accs: Vec<ColumnAcc>,
    row_count: u64,
    scanned: u64,
) -> TableProfile {
    let columns: Vec<ColumnProfile> = accs.into_iter().map(|a| a.finish(scanned)).collect();
    TableProfile {
        format: format.to_string(),
        row_count,
        scanned_rows: scanned,
        column_count: columns.len() as u32,
        columns,
        truncated: row_count > scanned,
    }
}

/// Renders a spreadsheet cell as the trimmed string used for profiling. Floats
/// that are whole numbers render without a trailing `.0` so they parse as ints.
fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.trim().to_string(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.is_finite() {
                format!("{}", *f as i64)
            } else {
                f.to_string()
            }
        }
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => dt.to_string(),
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("{e:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_csv_types_and_missing() {
        let csv = "id,city,score,joined\n\
                   1,Warsaw,3.5,2021-01-02\n\
                   2,Krakow,,2021-03-04\n\
                   3,Warsaw,4.0,2021-05-06\n\
                   4,Gdansk,2.5,2021-07-08\n";
        let p = profile_table(csv.as_bytes(), "people.csv").unwrap();
        assert_eq!(p.format, "csv");
        assert_eq!(p.row_count, 4);
        assert_eq!(p.column_count, 4);

        let city = p.columns.iter().find(|c| c.name == "city").unwrap();
        assert_eq!(city.column_type, ColumnType::Categorical);
        assert_eq!(city.unique_count, 3);
        assert_eq!(city.classes.len(), 3);
        assert_eq!(city.classes[0].value, "Warsaw");
        assert_eq!(city.classes[0].count, 2);

        let score = p.columns.iter().find(|c| c.name == "score").unwrap();
        assert_eq!(score.column_type, ColumnType::Float);
        assert!((score.missing_ratio - 0.25).abs() < 1e-9);

        let joined = p.columns.iter().find(|c| c.name == "joined").unwrap();
        assert_eq!(joined.column_type, ColumnType::Date);
    }

    #[test]
    fn rejects_empty_and_unknown() {
        assert!(profile_table(b"", "x.csv").is_err());
        assert!(profile_table(b"a,b\n1,2\n", "x.parquet").is_err());
    }

    #[test]
    fn integer_id_column_is_integer_not_categorical() {
        let mut csv = String::from("id\n");
        for i in 0..200 {
            csv.push_str(&format!("{i}\n"));
        }
        let p = profile_table(csv.as_bytes(), "ids.csv").unwrap();
        let id = &p.columns[0];
        assert_eq!(id.column_type, ColumnType::Integer);
    }
}
