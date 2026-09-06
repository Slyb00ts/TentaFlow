// ===== File: tentaquant/export.rs — the scientific package of one run =====
//
// Plan §13.6 asks for ONE `.zip` a person can attach to a paper: the raw
// numbers, the state vector in a form NumPy opens without a helper, the
// program that produced them, a methodological note and a BibTeX entry.
//
// Two rules shape everything here, and both are about trust:
//
//   * `method.md` IS GENERATED, NOT WRITTEN. Every line of it comes from a
//     stored row — the run, its metrics, its source. Nothing is inferred, no
//     number is recomputed for the note, and a fact the run did not store is
//     absent instead of guessed. That is what makes the note reproducible: run
//     the export twice on the same row and the note is identical.
//   * A PART IS WHOLE OR ABSENT. A state vector over the §18 decision 9
//     ceiling is a refusal of the whole export, never a truncated
//     `statevector.npz` — a file that opens and holds half a state is worse
//     than no file, because nothing downstream can tell.
//
// The archive is built with the `zip` crate, the same one the Project Studio
// archive uses; its own writers are private to that module and carry its
// manifest's `FileEntry`, so what is shared here is the crate, not a fork of
// its helpers.

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use num_complex::Complex64;
use tentaflow_protocol::tentaquant::{
    RunMetrics, RUN_EXPORT_PART_CIRCUIT_QASM, RUN_EXPORT_PART_CITATION_BIB,
    RUN_EXPORT_PART_COUNTS_CSV, RUN_EXPORT_PART_COUNTS_JSON, RUN_EXPORT_PART_METHOD_MD,
    RUN_EXPORT_PART_STATEVECTOR_NPZ,
};

use super::cas;
use super::circuit;
use super::db::RunRecord;
use super::runs::{self, StoredCounts, StoredState, MAX_STATE_ARTIFACT_BYTES};
use crate::db::DbPool;

/// Entry names inside the archive. They are the contract a reader's script
/// hard-codes, so they live here and nowhere else.
const ENTRY_COUNTS_JSON: &str = "counts.json";
const ENTRY_COUNTS_CSV: &str = "counts.csv";
const ENTRY_STATEVECTOR_NPZ: &str = "statevector.npz";
const ENTRY_CIRCUIT_QASM: &str = "circuit.qasm";
const ENTRY_METHOD_MD: &str = "method.md";
const ENTRY_CITATION_BIB: &str = "citation.bib";

/// The array inside `statevector.npz`. An `.npz` is a zip of `.npy` members
/// named after the arrays, so `numpy.load(...)["statevector"]` is what a
/// reader writes.
const NPZ_MEMBER: &str = "statevector.npy";

/// Everything the package is built from — all of it read from stored rows.
pub struct ExportInputs<'a> {
    pub run: &'a RunRecord,
    pub metrics: Option<RunMetrics>,
    pub counts: Option<StoredCounts>,
    pub state: Option<StoredState>,
    /// Display name of the person who started the run, for the BibTeX author.
    pub user_name: &'a str,
    pub project_name: Option<&'a str>,
}

/// The finished archive and exactly what went into it.
pub struct ExportPackage {
    pub bytes: Vec<u8>,
    pub entries: Vec<String>,
}

/// Why a package was not produced. The two are different ANSWERS: a refusal is
/// the caller's (a state over the §18 decision 9 ceiling), a fault is the
/// server's, and reporting a fault as a bad request would tell a person to fix
/// a request that was never wrong.
#[derive(Debug)]
pub enum ExportError {
    Refused(String),
    Internal(anyhow::Error),
}

/// Everything that fails with `?` inside the builder — a zip write, a JSON
/// encoding — is a fault of ours. A refusal is never raised this way: it is
/// constructed explicitly, so the two can never be confused by a `?` added
/// later.
impl From<anyhow::Error> for ExportError {
    fn from(error: anyhow::Error) -> Self {
        ExportError::Internal(error)
    }
}

/// The whole export as ONE blocking step: read the run's stored artifacts,
/// build the archive and put it in the content store.
///
/// It belongs on a blocking thread in one piece — the state artifact alone
/// reaches the storage ceiling, and parsing it, deflating it and writing it
/// back are all work no async reactor thread may do.
pub fn package(
    pool: &DbPool,
    data_dir: &Path,
    run: &RunRecord,
    metrics: Option<RunMetrics>,
    user_name: &str,
    project_name: Option<&str>,
    parts: &[String],
) -> std::result::Result<(String, u64, Vec<String>), ExportError> {
    let counts = runs::stored_counts(pool, data_dir, &run.id).map_err(ExportError::Internal)?;
    let state = runs::stored_state(pool, data_dir, &run.id).map_err(ExportError::Internal)?;
    let built = build(
        &ExportInputs {
            run,
            metrics,
            counts,
            state,
            user_name,
            project_name,
        },
        parts,
    )?;
    let size_bytes = built.bytes.len() as u64;
    let sha256 = cas::store_blob(data_dir, &built.bytes).map_err(ExportError::Internal)?;
    Ok((sha256, size_bytes, built.entries))
}

/// Builds the package. `parts` empty means every part the run has data for; a
/// named part the run has nothing for is absent from `entries` rather than
/// written empty.
pub fn build(
    inputs: &ExportInputs<'_>,
    parts: &[String],
) -> std::result::Result<ExportPackage, ExportError> {
    let wanted = |part: &str| parts.is_empty() || parts.iter().any(|p| p == part);

    // The one condition that fails the WHOLE export: a state over the §18
    // decision 9 ceiling. It is measured with `state_json_bytes`, the SAME
    // quantity the store gate uses, so both refuse exactly the same states —
    // measuring the packed `.npy` here instead would set a looser second
    // policy and accept a row the laboratory would never have written.
    // Checked before a single byte is written, so a refusal never leaves a
    // half-built archive behind.
    if wanted(RUN_EXPORT_PART_STATEVECTOR_NPZ) {
        if let Some(state) = &inputs.state {
            let bytes = circuit::state_json_bytes(state.num_qubits);
            if bytes > MAX_STATE_ARTIFACT_BYTES {
                return Err(ExportError::Refused(format!(
                    "the stored state vector of {} qubits is about {bytes} bytes, over the \
                     {MAX_STATE_ARTIFACT_BYTES} byte limit of a state artifact; export the run \
                     without '{RUN_EXPORT_PART_STATEVECTOR_NPZ}'",
                    state.num_qubits
                )));
            }
        }
    }

    let mut entries = Vec::new();
    let buffer = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buffer);

    if wanted(RUN_EXPORT_PART_COUNTS_JSON) {
        if let Some(counts) = &inputs.counts {
            write_entry(&mut zip, ENTRY_COUNTS_JSON, counts_json(counts)?.as_bytes())?;
            entries.push(ENTRY_COUNTS_JSON.to_string());
        }
    }
    if wanted(RUN_EXPORT_PART_COUNTS_CSV) {
        if let Some(counts) = &inputs.counts {
            write_entry(&mut zip, ENTRY_COUNTS_CSV, counts_csv(counts).as_bytes())?;
            entries.push(ENTRY_COUNTS_CSV.to_string());
        }
    }
    if wanted(RUN_EXPORT_PART_STATEVECTOR_NPZ) {
        if let Some(state) = &inputs.state {
            let npz = statevector_npz(&state.amplitudes, element_bytes(inputs))?;
            write_entry(&mut zip, ENTRY_STATEVECTOR_NPZ, &npz)?;
            entries.push(ENTRY_STATEVECTOR_NPZ.to_string());
        }
    }
    if wanted(RUN_EXPORT_PART_CIRCUIT_QASM) {
        if let Some(source) = inputs.run.source_qasm.as_deref() {
            write_entry(&mut zip, ENTRY_CIRCUIT_QASM, source.as_bytes())?;
            entries.push(ENTRY_CIRCUIT_QASM.to_string());
        }
    }
    if wanted(RUN_EXPORT_PART_METHOD_MD) {
        write_entry(&mut zip, ENTRY_METHOD_MD, method_md(inputs).as_bytes())?;
        entries.push(ENTRY_METHOD_MD.to_string());
    }
    if wanted(RUN_EXPORT_PART_CITATION_BIB) {
        write_entry(
            &mut zip,
            ENTRY_CITATION_BIB,
            citation_bib(inputs).as_bytes(),
        )?;
        entries.push(ENTRY_CITATION_BIB.to_string());
    }

    if entries.is_empty() {
        return Err(ExportError::Refused(
            "this run has nothing to export in the requested parts".to_string(),
        ));
    }
    let bytes = zip.finish().map_err(anyhow::Error::from)?.into_inner();
    Ok(ExportPackage { bytes, entries })
}

/// Deflated, like every other archive this repo writes: the members are text
/// and a state vector's JSON-free binary still compresses.
fn write_entry<W: Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    name: &str,
    bytes: &[u8],
) -> Result<()> {
    zip.start_file(
        name.to_string(),
        zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )?;
    zip.write_all(bytes)?;
    Ok(())
}

/// Bytes one amplitude occupies in the array: a run that computed in single
/// precision is written as `complex64`, because widening f32 results to
/// `complex128` claims a precision the numbers do not have.
fn element_bytes(inputs: &ExportInputs<'_>) -> usize {
    match inputs.metrics.as_ref().map(|m| m.precision.as_str()) {
        Some("single") => 8,
        _ => 16,
    }
}

fn counts_json(counts: &StoredCounts) -> Result<String> {
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "counts": counts.counts,
        "shots": counts.shots,
    }))?)
}

/// `bitstring,count,probability` — the three columns a spreadsheet and a
/// plotting script both expect. Bitstrings are `0`/`1` only, so no quoting
/// rule can ever apply to them.
fn counts_csv(counts: &StoredCounts) -> String {
    let total = counts.shots.max(1) as f64;
    let mut csv = String::from("bitstring,count,probability\n");
    for (bits, count) in &counts.counts {
        csv.push_str(&format!("{bits},{count},{}\n", *count as f64 / total));
    }
    csv
}

/// One `.npy` member inside a zip container — that IS the `.npz` format.
fn statevector_npz(amplitudes: &[Complex64], element: usize) -> Result<Vec<u8>> {
    let buffer = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buffer);
    write_entry(&mut zip, NPZ_MEMBER, &npy_complex(amplitudes, element))?;
    Ok(zip.finish()?.into_inner())
}

/// A minimal `.npy` version 1.0 writer for a 1-D complex array.
///
/// The format is a magic string, a version, a little-endian `u16` header
/// length and a Python dict literal terminated by a newline and padded with
/// spaces so that the DATA starts at a multiple of 64 bytes — that padding is
/// the whole reason a hand-rolled writer is worth a test: get it wrong and
/// NumPy reads the array shifted rather than refusing the file.
pub fn npy_complex(amplitudes: &[Complex64], element: usize) -> Vec<u8> {
    let descr = if element == 8 { "<c8" } else { "<c16" };
    let dict = format!(
        "{{'descr': '{descr}', 'fortran_order': False, 'shape': ({},), }}",
        amplitudes.len()
    );
    // magic(6) + version(2) + header length(2) + dict + '\n'
    let prefix = 6 + 2 + 2;
    let padding = (64 - (prefix + dict.len() + 1) % 64) % 64;
    let header = format!("{dict}{}\n", " ".repeat(padding));

    let mut out = Vec::with_capacity(prefix + header.len() + amplitudes.len() * element);
    out.extend_from_slice(&[0x93, b'N', b'U', b'M', b'P', b'Y', 1, 0]);
    out.extend_from_slice(&(header.len() as u16).to_le_bytes());
    out.extend_from_slice(header.as_bytes());
    for amplitude in amplitudes {
        if element == 8 {
            out.extend_from_slice(&(amplitude.re as f32).to_le_bytes());
            out.extend_from_slice(&(amplitude.im as f32).to_le_bytes());
        } else {
            out.extend_from_slice(&amplitude.re.to_le_bytes());
            out.extend_from_slice(&amplitude.im.to_le_bytes());
        }
    }
    out
}

fn field(md: &mut String, name: &str, value: &str) {
    md.push_str(&format!("| {name} | {value} |\n"));
}

/// The methodological note. Every row below reads one stored value; a run that
/// stored none of a section gets no section rather than a row of dashes.
fn method_md(inputs: &ExportInputs<'_>) -> String {
    let run = inputs.run;
    let mut md = format!("# Method note — run `{}`\n\n", run.id);
    md.push_str(
        "Generated from the stored record of this run. Every value below is read from the \
         laboratory's database; nothing is recomputed or inferred.\n\n",
    );

    md.push_str("## Execution\n\n| | |\n|---|---|\n");
    field(&mut md, "Run id", &format!("`{}`", run.id));
    field(&mut md, "Kind", &run.kind);
    if let Some(project) = inputs.project_name {
        field(&mut md, "Project", project);
    }
    field(&mut md, "Started", &run.started_at);
    if let Some(ended) = &run.ended_at {
        field(&mut md, "Ended", ended);
    }
    field(&mut md, "Status", &run.status);
    field(&mut md, "Target", &run.target);
    if let Some(node) = &run.node_id {
        field(&mut md, "Node", node);
    }

    if let Some(metrics) = &inputs.metrics {
        // A run that closed before the version was recorded has no engine to
        // name, and the note leaves the row out rather than naming the build
        // that happens to be packaging it.
        if !metrics.core_version.is_empty() {
            field(
                &mut md,
                "Engine",
                &format!("TentaFlow Core {}", metrics.core_version),
            );
        }
        md.push_str("\n## Simulation\n\n| | |\n|---|---|\n");
        field(&mut md, "Simulator backend", &metrics.backend);
        field(&mut md, "Method", &metrics.method);
        field(&mut md, "Amplitude precision", &metrics.precision);
        field(&mut md, "Qubits", &metrics.qubits.to_string());
        field(&mut md, "Classical bits", &metrics.clbits.to_string());
        field(&mut md, "Gates", &metrics.gates.to_string());
        field(&mut md, "Shots", &metrics.shots.to_string());
        field(&mut md, "Seed", &metrics.seed.to_string());
        field(&mut md, "Duration", &format!("{} ms", metrics.duration_ms));
        field(
            &mut md,
            "Peak state memory",
            &format!("{} B", metrics.memory_bytes),
        );
        field(
            &mut md,
            "Recorded evolution",
            &if metrics.keyframes > 0 {
                format!("yes, {} keyframes", metrics.keyframes)
            } else {
                "no".to_string()
            },
        );
        field(
            &mut md,
            "Stored state vector",
            if inputs.state.is_some() { "yes" } else { "no" },
        );
        // The two notes exist precisely so a missing artifact is explained
        // rather than silently absent; dropping them here would undo that.
        if let Some(note) = &metrics.evolution_note {
            field(&mut md, "Evolution note", note);
        }
        if let Some(note) = &metrics.state_note {
            field(&mut md, "State note", note);
        }
    }

    if let Some(counts) = &inputs.counts {
        md.push_str("\n## Measurement\n\n| | |\n|---|---|\n");
        field(&mut md, "Shots", &counts.shots.to_string());
        field(
            &mut md,
            "Distinct outcomes",
            &counts.counts.len().to_string(),
        );
        md.push_str("\nThe full histogram is in `counts.json` and `counts.csv`.\n");
    }
    if let Some(error) = &run.error {
        md.push_str(&format!("\n## Outcome\n\nThe run ended with: {error}\n"));
    }
    md
}

/// A BibTeX entry keyed by the run, citing the run id and the date it started
/// — the two facts that identify it. `note` repeats the target, because a run
/// is only reproducible together with the tier that executed it.
fn citation_bib(inputs: &ExportInputs<'_>) -> String {
    let run = inputs.run;
    let year = run.started_at.get(..4).unwrap_or("");
    format!(
        "@misc{{tentaquant-{id},\n  title  = {{TentaQuant run {id}}},\n  author = {{{author}}},\n  \
         year   = {{{year}}},\n  note   = {{Run {id}, started {started}, target {target}}},\n}}\n",
        id = run.id,
        author = bib_escape(inputs.user_name),
        started = run.started_at,
        target = run.target,
    )
}

/// BibTeX braces and backslashes are syntax, so a display name carrying one
/// would produce an entry no reader can parse.
fn bib_escape(value: &str) -> String {
    value
        .chars()
        .filter(|c| !matches!(c, '{' | '}' | '\\'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Read;

    fn counts() -> StoredCounts {
        StoredCounts {
            counts: BTreeMap::from([("00".to_string(), 512), ("11".to_string(), 512)]),
            shots: 1024,
        }
    }

    fn run_record() -> RunRecord {
        RunRecord {
            id: "run-1".to_string(),
            project_id: Some("p1".to_string()),
            notebook_id: None,
            cell_id: Some("c1".to_string()),
            kind: "circuit".to_string(),
            target: "core:node-a".to_string(),
            node_id: Some("node-a".to_string()),
            status: "succeeded".to_string(),
            started_at: "2026-09-05 10:00:00".to_string(),
            ended_at: Some("2026-09-05 10:00:01".to_string()),
            error: None,
            metrics_json: None,
            user_id: "u1".to_string(),
            pinned_at: None,
            tile_json: None,
            keyframes_sha256: None,
            source_qasm: Some("OPENQASM 3.0;\nqubit[2] q;\n".to_string()),
        }
    }

    fn metrics() -> RunMetrics {
        RunMetrics {
            duration_ms: 12,
            qubits: 2,
            clbits: 2,
            shots: 1024,
            memory_bytes: 64,
            gates: 2,
            keyframes: 2,
            method: "statevector".to_string(),
            precision: "double".to_string(),
            seed: 7,
            evolution_note: None,
            backend: "cpu".to_string(),
            core_version: "0.1.0".to_string(),
            state_note: None,
        }
    }

    /// The message of a refusal, so a test can assert on it without matching
    /// the whole enum every time.
    fn refusal(error: ExportError) -> String {
        match error {
            ExportError::Refused(why) => why,
            ExportError::Internal(error) => panic!("expected a refusal, got a fault: {error}"),
        }
    }

    fn read_entry(bytes: &[u8], name: &str) -> Vec<u8> {
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("archive opens");
        let mut entry = archive.by_name(name).expect("entry present");
        let mut out = Vec::new();
        entry.read_to_end(&mut out).expect("read entry");
        out
    }

    /// The header is the half of `.npy` a hand-rolled writer gets wrong: the
    /// dtype, the shape and — above all — the padding that puts the data at a
    /// multiple of 64 bytes. This decodes it the way NumPy does.
    #[test]
    fn the_npy_header_decodes_and_the_data_starts_aligned() {
        let amplitudes = vec![
            Complex64::new(0.5f64.sqrt(), 0.0),
            Complex64::new(0.0, 0.0),
            Complex64::new(0.0, 0.0),
            Complex64::new(0.5f64.sqrt(), 0.0),
        ];
        let npy = npy_complex(&amplitudes, 16);
        assert_eq!(&npy[..6], &[0x93, b'N', b'U', b'M', b'P', b'Y']);
        assert_eq!(&npy[6..8], &[1, 0]);
        let header_len = u16::from_le_bytes([npy[8], npy[9]]) as usize;
        let header = std::str::from_utf8(&npy[10..10 + header_len]).expect("utf-8 header");
        assert!(header.starts_with("{'descr': '<c16', 'fortran_order': False, 'shape': (4,), }"));
        assert!(header.ends_with('\n'));
        assert_eq!((10 + header_len) % 64, 0, "data must start 64-byte aligned");
        assert_eq!(npy.len(), 10 + header_len + 4 * 16);
        // The amplitudes themselves, little-endian pairs.
        let data = &npy[10 + header_len..];
        let first = f64::from_le_bytes(data[..8].try_into().expect("8 bytes"));
        assert!((first - 0.5f64.sqrt()).abs() < 1e-15);

        // Single precision writes half the bytes and says so in the dtype.
        let single = npy_complex(&amplitudes, 8);
        let single_len = u16::from_le_bytes([single[8], single[9]]) as usize;
        let single_header = std::str::from_utf8(&single[10..10 + single_len]).expect("utf-8");
        assert!(single_header.contains("'<c8'"));
        assert_eq!(single.len(), 10 + single_len + 4 * 8);
    }

    /// The whole package: the entry names a reader's script hard-codes, the
    /// nested `.npz`, and the note built only from stored values.
    #[test]
    fn the_package_carries_every_part_with_its_contract_name() {
        let run = run_record();
        let inputs = ExportInputs {
            run: &run,
            metrics: Some(metrics()),
            counts: Some(counts()),
            state: Some(StoredState {
                num_qubits: 2,
                amplitudes: vec![
                    Complex64::new(0.5f64.sqrt(), 0.0),
                    Complex64::new(0.0, 0.0),
                    Complex64::new(0.0, 0.0),
                    Complex64::new(0.5f64.sqrt(), 0.0),
                ],
            }),
            user_name: "Anna Kowalska",
            project_name: Some("Bell"),
        };
        let package = build(&inputs, &[]).expect("package builds");
        assert_eq!(
            package.entries,
            vec![
                "counts.json",
                "counts.csv",
                "statevector.npz",
                "circuit.qasm",
                "method.md",
                "citation.bib",
            ]
        );

        let csv = String::from_utf8(read_entry(&package.bytes, ENTRY_COUNTS_CSV)).expect("utf-8");
        assert_eq!(csv, "bitstring,count,probability\n00,512,0.5\n11,512,0.5\n");

        // The nested container really is a zip holding one `.npy` member.
        let npz = read_entry(&package.bytes, ENTRY_STATEVECTOR_NPZ);
        let member = read_entry(&npz, NPZ_MEMBER);
        assert_eq!(&member[..6], &[0x93, b'N', b'U', b'M', b'P', b'Y']);

        let qasm = read_entry(&package.bytes, ENTRY_CIRCUIT_QASM);
        assert_eq!(qasm, run.source_qasm.as_deref().expect("source").as_bytes());

        let note = String::from_utf8(read_entry(&package.bytes, ENTRY_METHOD_MD)).expect("utf-8");
        for expected in [
            "| Run id | `run-1` |",
            "| Target | core:node-a |",
            "| Node | node-a |",
            "| Engine | TentaFlow Core 0.1.0 |",
            "| Simulator backend | cpu |",
            "| Shots | 1024 |",
            "| Seed | 7 |",
            "| Qubits | 2 |",
            "| Recorded evolution | yes, 2 keyframes |",
            "| Stored state vector | yes |",
            "| Project | Bell |",
        ] {
            assert!(note.contains(expected), "method.md is missing: {expected}");
        }

        let bib = String::from_utf8(read_entry(&package.bytes, ENTRY_CITATION_BIB)).expect("utf-8");
        assert!(bib.contains("@misc{tentaquant-run-1,"));
        assert!(bib.contains("year   = {2026},"));
        assert!(bib.contains("started 2026-09-05 10:00:00"));
        assert!(bib.contains("author = {Anna Kowalska}"));
    }

    /// A run without a state and without counts still exports its note; the
    /// parts it has no data for are absent rather than empty files.
    #[test]
    fn a_part_without_data_is_absent_and_a_selection_is_honoured() {
        let run = run_record();
        let inputs = ExportInputs {
            run: &run,
            metrics: Some(metrics()),
            counts: None,
            state: None,
            user_name: "Anna",
            project_name: None,
        };
        let package = build(&inputs, &[]).expect("package builds");
        assert_eq!(
            package.entries,
            vec!["circuit.qasm", "method.md", "citation.bib"]
        );
        let note = String::from_utf8(read_entry(&package.bytes, ENTRY_METHOD_MD)).expect("utf-8");
        assert!(note.contains("| Stored state vector | no |"));

        let one =
            build(&inputs, &[RUN_EXPORT_PART_METHOD_MD.to_string()]).expect("selection builds");
        assert_eq!(one.entries, vec!["method.md"]);
    }

    /// The size rule of §18 decision 9: over the ceiling the WHOLE export is
    /// refused, so no archive can ever hold a truncated state vector. The
    /// ceiling is the store's own — a state this large is one the laboratory
    /// would not have written, so the row can only come from somewhere the
    /// store gate did not run, and the export refuses it rather than packing
    /// whatever bytes it holds.
    #[test]
    fn a_state_over_the_ceiling_refuses_the_whole_export() {
        let run = run_record();
        // The smallest register whose JSON artifact is over the ceiling; the
        // amplitudes are not read by the gate, so the test does not allocate
        // the 64 MiB the row would really carry.
        let num_qubits = (0u32..)
            .find(|q| circuit::state_json_bytes(*q) > MAX_STATE_ARTIFACT_BYTES)
            .expect("some register exceeds the ceiling");
        let inputs = ExportInputs {
            run: &run,
            metrics: Some(metrics()),
            counts: Some(counts()),
            state: Some(StoredState {
                num_qubits,
                amplitudes: vec![Complex64::new(0.0, 0.0); 4],
            }),
            user_name: "Anna",
            project_name: None,
        };
        let why = refusal(build(&inputs, &[]).map(|_| ()).expect_err("refused"));
        assert!(why.contains("over the"), "{why}");
        // The same run exports fine without that part.
        let without = build(&inputs, &[RUN_EXPORT_PART_COUNTS_JSON.to_string()])
            .expect("builds without the state");
        assert_eq!(without.entries, vec!["counts.json"]);
    }

    /// The store gate and the export gate refuse the SAME registers. A state
    /// the laboratory wrote can therefore always be exported, and no state it
    /// refused can slip through the package.
    #[test]
    fn the_export_ceiling_matches_the_one_the_store_applies() {
        let largest_stored = (0u32..)
            .take_while(|q| circuit::state_json_bytes(*q) <= MAX_STATE_ARTIFACT_BYTES)
            .last()
            .expect("some register fits");
        let run = run_record();
        let state = |num_qubits| {
            Some(StoredState {
                num_qubits,
                amplitudes: vec![Complex64::new(1.0, 0.0)],
            })
        };
        let inputs = |num_qubits| ExportInputs {
            run: &run,
            metrics: Some(metrics()),
            counts: None,
            state: state(num_qubits),
            user_name: "Anna",
            project_name: None,
        };
        assert!(build(&inputs(largest_stored), &[]).is_ok());
        assert!(build(&inputs(largest_stored + 1), &[]).is_err());
    }

    /// A refusal and a fault are different answers, and the builder must not
    /// blur them: asking for nothing the run has is the caller's mistake.
    #[test]
    fn an_empty_selection_is_a_refusal_not_a_fault() {
        let run = RunRecord {
            source_qasm: None,
            ..run_record()
        };
        let inputs = ExportInputs {
            run: &run,
            metrics: Some(metrics()),
            counts: None,
            state: None,
            user_name: "Anna",
            project_name: None,
        };
        let why = refusal(
            build(&inputs, &[RUN_EXPORT_PART_COUNTS_CSV.to_string()])
                .map(|_| ())
                .expect_err("refused"),
        );
        assert!(why.contains("nothing to export"), "{why}");
    }
}
