// ===== File: tentaquant/examples.rs — the example gallery: embedded circuits with a reference outcome =====
//
// An example is a ready circuit with the text that explains it (plan §12.1).
// The catalog is compiled into the binary like the katas, so every node of a
// fleet ships the same gallery and a lab cannot edit what an example claims.
//
// Layout, one directory per example under `examples/`:
//   example.toml    id, level, tags, titles and descriptions (pl + en), qubit range
//   README.md       the explanation, one `@@ <lang>` section per language
//   circuit.qasm    the canonical OpenQASM 3 program
//   expected.json   what a correct run looks like: shots, seed, tolerance, outcomes
//
// Only the variants that run today are shipped: the circuit runs on T0 and T1,
// and the Python / GPU / QPU variants of plan §12.1 arrive with the tiers that
// can execute them. A file that has no runtime must not exist as a promise.
//
// A parametric example (GHZ) ships `circuit.qasm` at its default width, so the
// file is a program the Studio opens as it stands. The front end takes literal
// register sizes only, hence the width is not a program parameter: the lines of
// the file that carry it are listed in `example.toml` (`[[width_rewrite]]`) and
// `qasm_at` rewrites them for any other width of the range. `load` proves the
// list is complete enough to be trusted — every line exists once and rewriting
// at the default width gives the file back byte for byte.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;
use tentaflow_protocol::tentaquant::{ExampleExpected, ExampleInfo};
use tentaflow_quantum::ir::OpKind;
use tentaflow_quantum::Circuit;

use super::circuit;
use super::kata::language_sections;

/// The languages every title, description and README must carry.
const REQUIRED_LANGUAGES: [&str; 2] = ["pl", "en"];

const LEVELS: [&str; 3] = ["intro", "core", "advanced"];

/// The raw files of one example, as compiled in.
struct Source {
    id: &'static str,
    example_toml: &'static str,
    readme_md: &'static str,
    circuit_qasm: &'static str,
    expected_json: &'static str,
}

macro_rules! sources {
    ($($id:literal),+ $(,)?) => {
        &[$(Source {
            id: $id,
            example_toml: include_str!(concat!("examples/", $id, "/example.toml")),
            readme_md: include_str!(concat!("examples/", $id, "/README.md")),
            circuit_qasm: include_str!(concat!("examples/", $id, "/circuit.qasm")),
            expected_json: include_str!(concat!("examples/", $id, "/expected.json")),
        }),+]
    };
}

/// Shipped order. The gallery lists them exactly like this.
const SOURCES: &[Source] = sources!["bell-state", "ghz"];

#[derive(Deserialize)]
struct ManifestFile {
    id: String,
    level: String,
    #[serde(default)]
    tags: Vec<String>,
    title: BTreeMap<String, String>,
    description: BTreeMap<String, String>,
    qubits: QubitRange,
    #[serde(default)]
    width_rewrite: Vec<WidthRewrite>,
}

/// One line of `circuit.qasm` that carries the register width and what it
/// becomes at another width.
#[derive(Debug, Clone, Deserialize)]
struct WidthRewrite {
    line: String,
    becomes: String,
}

#[derive(Deserialize)]
struct QubitRange {
    min: u32,
    max: u32,
    default: u32,
}

#[derive(Deserialize)]
struct ExpectedFile {
    shots: u64,
    seed: u64,
    tolerance: f64,
    /// `"qubits"`: every key of `outcomes` is a single bit that the width
    /// repeats (`"0"` at 5 qubits is `"00000"`). Absent: keys are literal.
    #[serde(default)]
    fill: Option<String>,
    outcomes: BTreeMap<String, f64>,
}

#[derive(Debug, Clone)]
pub struct Example {
    pub id: String,
    /// 1-based position in the shipped order.
    pub position: u32,
    pub level: String,
    pub tags: Vec<String>,
    pub titles: BTreeMap<String, String>,
    pub descriptions: BTreeMap<String, String>,
    pub readme: BTreeMap<String, String>,
    pub qubits_min: u32,
    pub qubits_max: u32,
    pub qubits_default: u32,
    circuit: String,
    width_rewrite: Vec<WidthRewrite>,
    shots: u64,
    seed: u64,
    tolerance: f64,
    fill_to_width: bool,
    outcomes: BTreeMap<String, f64>,
}

#[derive(Debug)]
pub struct Catalog {
    pub examples: Vec<Example>,
}

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    // Compiled in and loaded by a test, so a failure here is a defect of the
    // build, never a state a running node can be in.
    Catalog::load()
        .unwrap_or_else(|reason| panic!("the embedded example catalog is invalid: {reason}"))
});

pub fn catalog() -> &'static Catalog {
    &CATALOG
}

/// Layers of the circuit when each operation sits as early as its qubits allow.
/// A barrier only orders, so it is not a layer of its own.
pub fn depth(circuit: &Circuit) -> u32 {
    let mut busy_until = vec![0u32; circuit.num_qubits()];
    let mut depth = 0;
    for op in circuit.ops() {
        if matches!(op.kind, OpKind::Barrier { .. }) {
            continue;
        }
        let qubits = op.qubits();
        if qubits.is_empty() {
            continue;
        }
        let layer = qubits.iter().map(|&q| busy_until[q]).max().unwrap_or(0) + 1;
        for &q in qubits {
            busy_until[q] = layer;
        }
        depth = depth.max(layer);
    }
    depth
}

impl Example {
    pub fn is_parametric(&self) -> bool {
        self.qubits_min != self.qubits_max
    }

    /// The width a request asks for. `None` is the default; a width outside the
    /// range, or any width other than the only one of a fixed circuit, is
    /// refused with the sentence the caller shows.
    pub fn width(&self, requested: Option<u32>) -> Result<u32, String> {
        let Some(width) = requested else {
            return Ok(self.qubits_default);
        };
        if !self.is_parametric() {
            return if width == self.qubits_min {
                Ok(width)
            } else {
                Err(format!(
                    "example '{}' has a fixed width of {} qubits",
                    self.id, self.qubits_min
                ))
            };
        }
        if (self.qubits_min..=self.qubits_max).contains(&width) {
            Ok(width)
        } else {
            Err(format!(
                "example '{}' takes {} to {} qubits",
                self.id, self.qubits_min, self.qubits_max
            ))
        }
    }

    /// The canonical program at `width` (already validated by [`Self::width`]).
    pub fn qasm_at(&self, width: u32) -> String {
        if !self.is_parametric() {
            return self.circuit.clone();
        }
        self.width_rewrite
            .iter()
            .fold(self.circuit.clone(), |text, rewrite| {
                let becomes = rewrite
                    .becomes
                    .replace("{n}", &width.to_string())
                    .replace("{last}", &(width - 1).to_string());
                text.replacen(&rewrite.line, &becomes, 1)
            })
    }

    /// The reference outcome at `width`.
    pub fn expected_at(&self, width: u32) -> ExampleExpected {
        let outcomes = self
            .outcomes
            .iter()
            .map(|(bits, p)| {
                let key = if self.fill_to_width {
                    bits.repeat(width as usize)
                } else {
                    bits.clone()
                };
                (key, *p)
            })
            .collect();
        ExampleExpected {
            shots: self.shots,
            seed: self.seed,
            tolerance: self.tolerance,
            outcomes,
        }
    }

    /// The wire row at `width`. Qubits and depth come from parsing the very
    /// program a fork would hold.
    pub fn info(&self, width: u32) -> Result<ExampleInfo, String> {
        let parsed = circuit::parse(&self.qasm_at(width), "")
            .map_err(|d| format!("example '{}' does not parse: {}", self.id, d.message))?;
        Ok(ExampleInfo {
            example_id: self.id.clone(),
            position: self.position,
            titles: self.titles.clone(),
            descriptions: self.descriptions.clone(),
            level: self.level.clone(),
            tags: self.tags.clone(),
            qubits: parsed.circuit.num_qubits() as u32,
            qubits_min: self.qubits_min,
            qubits_max: self.qubits_max,
            qubits_default: self.qubits_default,
            depth: depth(&parsed.circuit),
        })
    }
}

impl Catalog {
    pub fn example(&self, id: &str) -> Option<&Example> {
        self.examples.iter().find(|e| e.id == id)
    }

    fn load() -> Result<Catalog, String> {
        let mut examples: Vec<Example> = Vec::with_capacity(SOURCES.len());
        for (index, source) in SOURCES.iter().enumerate() {
            let id = source.id;
            let manifest: ManifestFile = toml::from_str(source.example_toml)
                .map_err(|e| format!("{id}: example.toml: {e}"))?;
            if manifest.id != id {
                return Err(format!("{id}: example.toml names itself '{}'", manifest.id));
            }
            if examples.iter().any(|e| e.id == id) {
                return Err(format!("{id}: listed twice"));
            }
            if !LEVELS.contains(&manifest.level.as_str()) {
                return Err(format!("{id}: unknown level '{}'", manifest.level));
            }
            let QubitRange { min, max, default } = manifest.qubits;
            if min == 0 || min > default || default > max {
                return Err(format!(
                    "{id}: qubit range {min}..={max} does not hold default {default}"
                ));
            }
            if max > circuit::MAX_CORE_QUBITS {
                return Err(format!(
                    "{id}: {max} qubits is above what Core can simulate"
                ));
            }
            let readme = language_sections(id, "README.md", source.readme_md)?;
            for language in REQUIRED_LANGUAGES {
                for (what, texts) in [
                    ("title", &manifest.title),
                    ("description", &manifest.description),
                    ("README", &readme),
                ] {
                    if texts.get(language).map_or(true, |t| t.trim().is_empty()) {
                        return Err(format!("{id}: no {what} in '{language}'"));
                    }
                }
            }
            let expected: ExpectedFile = serde_json::from_str(source.expected_json)
                .map_err(|e| format!("{id}: expected.json: {e}"))?;
            let fill_to_width = match expected.fill.as_deref() {
                None => false,
                Some("qubits") => true,
                Some(other) => return Err(format!("{id}: unknown fill '{other}'")),
            };
            let total: f64 = expected.outcomes.values().sum();
            if (total - 1.0).abs() > 1e-9 {
                return Err(format!("{id}: expected outcomes sum to {total}, not 1"));
            }
            if expected.shots == 0 || !(expected.tolerance > 0.0 && expected.tolerance < 1.0) {
                return Err(format!(
                    "{id}: expected.json needs shots > 0 and a tolerance in (0, 1)"
                ));
            }
            let parametric = min != max;
            if parametric == manifest.width_rewrite.is_empty() {
                return Err(format!(
                    "{id}: a parametric example needs [[width_rewrite]] lines and a fixed one has none"
                ));
            }
            for rewrite in &manifest.width_rewrite {
                if source.circuit_qasm.matches(&rewrite.line).count() != 1 {
                    return Err(format!(
                        "{id}: circuit.qasm must hold the line `{}` exactly once",
                        rewrite.line
                    ));
                }
            }
            let example = Example {
                id: id.to_string(),
                position: index as u32 + 1,
                level: manifest.level,
                tags: manifest.tags,
                titles: manifest.title,
                descriptions: manifest.description,
                readme,
                qubits_min: min,
                qubits_max: max,
                qubits_default: default,
                circuit: source.circuit_qasm.to_string(),
                width_rewrite: manifest.width_rewrite,
                shots: expected.shots,
                seed: expected.seed,
                tolerance: expected.tolerance,
                fill_to_width,
                outcomes: expected.outcomes,
            };
            // The default must be a program the front end accepts at the very
            // width the manifest advertises.
            if example.qasm_at(default) != source.circuit_qasm {
                return Err(format!(
                    "{id}: rewriting circuit.qasm at its default width of {default} changes it"
                ));
            }
            let info = example.info(default)?;
            if info.qubits != default {
                return Err(format!(
                    "{id}: the circuit declares {} qubits, the manifest {default}",
                    info.qubits
                ));
            }
            examples.push(example);
        }
        Ok(Catalog { examples })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_quantum::grade::total_variation_distance;
    use tentaflow_quantum::sim::statevector::{run, SimOptions};
    use tentaflow_quantum::sim::{Cancel, Device, Precision};

    /// Widths a test actually simulates. The top of the range is only parsed:
    /// 2^28 amplitudes is a benchmark, not a unit test.
    fn simulated_widths(example: &Example) -> Vec<u32> {
        let mut widths = vec![example.qubits_min, example.qubits_default];
        if example.is_parametric() {
            widths.push(12);
        }
        widths.sort_unstable();
        widths.dedup();
        widths
    }

    #[test]
    fn the_embedded_catalog_loads_in_shipped_order() {
        let ids: Vec<&str> = catalog().examples.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["bell-state", "ghz"]);
        assert_eq!(catalog().examples[1].position, 2);
    }

    #[test]
    fn every_example_runs_and_lands_on_its_reference_outcome() {
        for example in &catalog().examples {
            for width in simulated_widths(example) {
                let parsed = circuit::parse(&example.qasm_at(width), "")
                    .unwrap_or_else(|d| panic!("{} at {width}: {}", example.id, d.message));
                assert_eq!(parsed.circuit.num_qubits() as u32, width, "{}", example.id);

                let expected = example.expected_at(width);
                let result = run(
                    &parsed.circuit,
                    &SimOptions {
                        precision: Precision::Double,
                        max_qubits: width as usize,
                        seed: expected.seed,
                    },
                    Device::Cpu,
                    expected.shots,
                    Cancel::none(),
                )
                .unwrap_or_else(|e| panic!("{} at {width}: {e}", example.id));
                assert_eq!(result.shots, expected.shots);

                // Every outcome the run produced is one the theory allows: a
                // stray bitstring is a wrong circuit, not shot noise.
                for (bits, count) in &result.counts {
                    assert!(
                        *count == 0 || expected.outcomes.contains_key(bits),
                        "{} at {width}: unexpected outcome {bits}",
                        example.id
                    );
                }
                let ideal: BTreeMap<String, u64> = expected
                    .outcomes
                    .iter()
                    .map(|(bits, p)| (bits.clone(), (p * 1e9).round() as u64))
                    .collect();
                let tvd = total_variation_distance(&result.counts, &ideal).expect("tvd");
                assert!(
                    tvd < expected.tolerance,
                    "{} at {width}: tvd {tvd} is not below {}",
                    example.id,
                    expected.tolerance
                );
            }
        }
    }

    #[test]
    fn a_parametric_example_is_rewritten_at_the_ends_of_its_range() {
        let ghz = catalog().example("ghz").expect("ghz");
        assert_eq!(ghz.width(None), Ok(5));
        assert_eq!(ghz.width(Some(3)), Ok(3));
        assert_eq!(ghz.width(Some(28)), Ok(28));
        assert!(ghz.width(Some(2)).is_err());
        assert!(ghz.width(Some(29)).is_err());
        for width in [ghz.qubits_min, ghz.qubits_max] {
            let info = ghz.info(width).expect("info");
            assert_eq!(info.qubits, width);
            // H, then one CNOT per other qubit — all of them through qubit 0 —
            // then the measurement layer.
            assert_eq!(info.depth, width + 1);
        }
        let nine = ghz.qasm_at(9);
        assert!(nine.contains("qubit[9] q;") && nine.contains("bit[9] c;"));
        assert!(nine.contains("[1:8]"));
        assert!(!nine.contains("[5]") && !nine.contains("[1:4]"));
        let expected = ghz.expected_at(4);
        assert_eq!(
            expected
                .outcomes
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["0000", "1111"]
        );
    }

    #[test]
    fn a_fixed_example_refuses_another_width() {
        let bell = catalog().example("bell-state").expect("bell");
        assert_eq!(bell.width(None), Ok(2));
        assert_eq!(bell.width(Some(2)), Ok(2));
        assert!(bell.width(Some(3)).is_err());
        let info = bell.info(2).expect("info");
        assert_eq!((info.qubits, info.depth), (2, 3));
        assert_eq!(depth(&Circuit::new()), 0);
    }
}
