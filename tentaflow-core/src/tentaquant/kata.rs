// ===== File: tentaquant/kata.rs — the course: embedded katas, grading, unlocking, ranking =====
//
// A kata is a short task with an automatic verdict (plan §12.3). The whole
// catalog is compiled into the binary, so a node grades exactly the course it
// shipped with and no lab can edit what "passed" means.
//
// Three properties decide the shape of this module:
//
//   * GRADING IS DETERMINISTIC. The same program gets the same verdict on every
//     node and every run: states and unitaries are compared analytically, and a
//     distribution kata samples with the seed its `verify.toml` fixes. There is
//     no Python and no wall-clock dependence, which is also what keeps a verdict
//     under the 100 ms the plan asks for.
//   * THE FRONT END IS THE SAME ONE the editor and the T1 runs use
//     (`circuit::parse`), so a program the Studio accepts is graded, and one it
//     rejects comes back as the same diagnostic with the same line.
//   * PROGRESS IS PURE ARITHMETIC over a small row. `record` and `ranking` take
//     rows and return rows; the database layer only stores what they decide, so
//     "points are awarded once" is a property of one function, not of a query.
//
// Catalog layout, one directory per kata under `katas/`:
//   kata.toml      id, points, tier, titles and summaries (pl + en)
//   task.md        the task text, one `@@ <lang>` section per language
//   task.qasm      the skeleton the editor starts from; it must NOT pass
//   verify.toml    `state_equals` | `unitary_equals` | `counts_tvd_below`
//   solution.qasm  the reference answer; it must pass (a test enforces both)
// `groups.toml` fixes the order: the groups, and the katas inside each.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;
use std::time::Instant;

use num_complex::Complex64;
use serde::Deserialize;
use tentaflow_protocol::tentaquant::{
    CircuitDiagnostic, KataGrade, KataGroupInfo, KataInfo, KATA_RANKING_TOP,
};
use tentaflow_quantum::grade as quantum_grade;
use tentaflow_quantum::sim::statevector::{
    circuit_unitary, require_unitary, run, statevector, SimOptions,
};
use tentaflow_quantum::sim::{Cancel, Device, Precision};
use tentaflow_quantum::Circuit;

use super::circuit;

pub const OUTCOME_PASSED: &str = "passed";
pub const OUTCOME_FAILED: &str = "failed";
pub const OUTCOME_INVALID: &str = "invalid";

pub const STATUS_LOCKED: &str = "locked";
pub const STATUS_OPEN: &str = "open";
pub const STATUS_ATTEMPTED: &str = "attempted";
pub const STATUS_PASSED: &str = "passed";

const METRIC_FIDELITY: &str = "fidelity";
const METRIC_PROCESS_FIDELITY: &str = "process_fidelity";
const METRIC_TVD: &str = "tvd";

/// Registers a kata grading may allocate. A kata is a few qubits; the ceiling
/// only has to stop `qubit[30] q;` from reaching an allocation.
const KATA_MAX_QUBITS: usize = 12;

/// The languages every title, summary and task text must carry.
const REQUIRED_LANGUAGES: [&str; 2] = ["pl", "en"];

/// The raw files of one kata, as compiled in.
struct Source {
    id: &'static str,
    kata_toml: &'static str,
    task_md: &'static str,
    task_qasm: &'static str,
    verify_toml: &'static str,
    solution_qasm: &'static str,
}

macro_rules! sources {
    ($($id:literal),+ $(,)?) => {
        &[$(Source {
            id: $id,
            kata_toml: include_str!(concat!("katas/", $id, "/kata.toml")),
            task_md: include_str!(concat!("katas/", $id, "/task.md")),
            task_qasm: include_str!(concat!("katas/", $id, "/task.qasm")),
            verify_toml: include_str!(concat!("katas/", $id, "/verify.toml")),
            solution_qasm: include_str!(concat!("katas/", $id, "/solution.qasm")),
        }),+]
    };
}

const SOURCES: &[Source] = sources![
    "01-superposition-h",
    "02-x-gate",
    "03-rotations",
    "04-bloch-sphere",
    "05-phase-gates",
    "06-x-basis-measurement",
    "07-two-qubit-product",
    "08-cnot",
    "09-cz-from-cnot",
    "10-bell-state",
];

const GROUPS_TOML: &str = include_str!("katas/groups.toml");

// ---------------------------------------------------------------------------
// The catalog
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct GroupsFile {
    group: Vec<GroupSpec>,
}

#[derive(Deserialize)]
struct GroupSpec {
    id: String,
    title: BTreeMap<String, String>,
    katas: Vec<String>,
}

#[derive(Deserialize)]
struct KataFile {
    id: String,
    points: u32,
    tier: String,
    title: BTreeMap<String, String>,
    summary: BTreeMap<String, String>,
}

/// What a submission is held to. The target is stored as the plain numbers the
/// `verify.toml` carries, so the file is the specification a reviewer reads.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum VerifyFile {
    StateEquals {
        tolerance: f64,
        /// `[re, im]` pairs in basis order, qubit 0 the rightmost bit.
        target: Vec<[f64; 2]>,
    },
    UnitaryEquals {
        tolerance: f64,
        /// Rows of `[re, im]` pairs, columns in the same basis order.
        target: Vec<Vec<[f64; 2]>>,
    },
    CountsTvdBelow {
        shots: u64,
        seed: u64,
        max_tvd: f64,
        /// An outcome the ideal distribution gives probability zero must not be
        /// observed at all, however small the distance stays.
        #[serde(default)]
        exact_support: bool,
        expected: BTreeMap<String, f64>,
    },
}

#[derive(Debug, Clone)]
enum Verify {
    State {
        tolerance: f64,
        target: Vec<Complex64>,
        qubits: usize,
    },
    Unitary {
        tolerance: f64,
        target: Vec<Complex64>,
        qubits: usize,
    },
    Counts {
        shots: u64,
        seed: u64,
        max_tvd: f64,
        exact_support: bool,
        expected: BTreeMap<String, f64>,
        width: usize,
    },
}

#[derive(Debug, Clone)]
pub struct Kata {
    pub id: String,
    /// Index into [`Catalog::groups`].
    pub group: usize,
    /// 1-based position in the whole course.
    pub position: u32,
    pub points: u32,
    pub tier: String,
    pub titles: BTreeMap<String, String>,
    pub summaries: BTreeMap<String, String>,
    pub task: BTreeMap<String, String>,
    pub starter: String,
    pub solution: String,
    verify: Verify,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub id: String,
    pub titles: BTreeMap<String, String>,
    /// Indices into [`Catalog::katas`], in course order.
    pub katas: Vec<usize>,
}

#[derive(Debug)]
pub struct Catalog {
    pub groups: Vec<Group>,
    pub katas: Vec<Kata>,
}

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    // The catalog is compiled in and a test loads it, so a failure here is a
    // defect of the build, not a state a running node can be in.
    Catalog::load()
        .unwrap_or_else(|reason| panic!("the embedded kata catalog is invalid: {reason}"))
});

pub fn catalog() -> &'static Catalog {
    &CATALOG
}

/// Splits `task.md` into its `@@ <lang>` sections.
fn task_sections(id: &str, text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut sections: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if let Some(language) = line.strip_prefix("@@ ") {
            let language = language.trim().to_string();
            if sections.insert(language.clone(), String::new()).is_some() {
                return Err(format!("{id}: task.md repeats the section '{language}'"));
            }
            current = Some(language);
        } else if let Some(language) = &current {
            let body = sections.entry(language.clone()).or_default();
            body.push_str(line);
            body.push('\n');
        } else if !line.trim().is_empty() {
            return Err(format!("{id}: task.md has text before its first section"));
        }
    }
    for body in sections.values_mut() {
        *body = body.trim().to_string();
    }
    Ok(sections)
}

fn complex(pair: [f64; 2]) -> Complex64 {
    Complex64::new(pair[0], pair[1])
}

/// log2 of a power of two, or an error naming what was being measured.
fn log2_exact(id: &str, what: &str, n: usize) -> Result<usize, String> {
    if n < 2 || !n.is_power_of_two() {
        return Err(format!("{id}: {what} has {n} entries, not a power of two"));
    }
    Ok(n.trailing_zeros() as usize)
}

fn verify_of(id: &str, text: &str) -> Result<Verify, String> {
    let file: VerifyFile = toml::from_str(text).map_err(|e| format!("{id}: verify.toml: {e}"))?;
    match file {
        VerifyFile::StateEquals { tolerance, target } => {
            let qubits = log2_exact(id, "the target state", target.len())?;
            let target: Vec<Complex64> = target.into_iter().map(complex).collect();
            let norm: f64 = target.iter().map(Complex64::norm_sqr).sum();
            if (norm - 1.0).abs() > 1e-9 {
                return Err(format!("{id}: the target state has norm² {norm}, not 1"));
            }
            Ok(Verify::State {
                tolerance,
                target,
                qubits,
            })
        }
        VerifyFile::UnitaryEquals { tolerance, target } => {
            let dim = target.len();
            let qubits = log2_exact(id, "the target unitary", dim)?;
            if target.iter().any(|row| row.len() != dim) {
                return Err(format!("{id}: the target unitary is not square"));
            }
            let target: Vec<Complex64> = target.into_iter().flatten().map(complex).collect();
            Ok(Verify::Unitary {
                tolerance,
                target,
                qubits,
            })
        }
        VerifyFile::CountsTvdBelow {
            shots,
            seed,
            max_tvd,
            exact_support,
            expected,
        } => {
            if shots == 0 {
                return Err(format!("{id}: a distribution kata needs at least one shot"));
            }
            let width = expected.keys().next().map(String::len).unwrap_or(0);
            if width == 0
                || expected
                    .keys()
                    .any(|key| key.len() != width || !key.chars().all(|c| c == '0' || c == '1'))
            {
                return Err(format!(
                    "{id}: expected outcomes must be bitstrings of one width"
                ));
            }
            let total: f64 = expected.values().sum();
            if (total - 1.0).abs() > 1e-9 {
                return Err(format!(
                    "{id}: expected probabilities sum to {total}, not 1"
                ));
            }
            Ok(Verify::Counts {
                shots,
                seed,
                max_tvd,
                exact_support,
                expected,
                width,
            })
        }
    }
}

fn require_languages(id: &str, what: &str, map: &BTreeMap<String, String>) -> Result<(), String> {
    for language in REQUIRED_LANGUAGES {
        if map
            .get(language)
            .map_or(true, |text| text.trim().is_empty())
        {
            return Err(format!("{id}: {what} has no '{language}' text"));
        }
    }
    Ok(())
}

impl Catalog {
    fn load() -> Result<Catalog, String> {
        let groups_file: GroupsFile =
            toml::from_str(GROUPS_TOML).map_err(|e| format!("groups.toml: {e}"))?;
        let mut parsed: HashMap<&str, (KataFile, &Source)> = HashMap::new();
        for source in SOURCES {
            let file: KataFile = toml::from_str(source.kata_toml)
                .map_err(|e| format!("{}: kata.toml: {e}", source.id))?;
            if file.id != source.id {
                return Err(format!(
                    "{}: kata.toml names itself '{}'",
                    source.id, file.id
                ));
            }
            parsed.insert(source.id, (file, source));
        }

        let mut groups = Vec::new();
        let mut katas: Vec<Kata> = Vec::new();
        for (group_index, spec) in groups_file.group.into_iter().enumerate() {
            require_languages(&spec.id, "the group title", &spec.title)?;
            if spec.katas.is_empty() {
                return Err(format!("group '{}' holds no kata", spec.id));
            }
            let mut members = Vec::new();
            for id in &spec.katas {
                let (file, source) = parsed.remove(id.as_str()).ok_or_else(|| {
                    format!(
                        "group '{}' names '{id}', which is not shipped or is listed twice",
                        spec.id
                    )
                })?;
                require_languages(id, "the title", &file.title)?;
                require_languages(id, "the summary", &file.summary)?;
                let task = task_sections(id, source.task_md)?;
                require_languages(id, "the task", &task)?;
                if file.points == 0 {
                    return Err(format!("{id}: a kata is worth at least one point"));
                }
                members.push(katas.len());
                katas.push(Kata {
                    id: file.id,
                    group: group_index,
                    position: katas.len() as u32 + 1,
                    points: file.points,
                    tier: file.tier,
                    titles: file.title,
                    summaries: file.summary,
                    task,
                    starter: source.task_qasm.to_string(),
                    solution: source.solution_qasm.to_string(),
                    verify: verify_of(id, source.verify_toml)?,
                });
            }
            groups.push(Group {
                id: spec.id,
                titles: spec.title,
                katas: members,
            });
        }
        if let Some(orphan) = parsed.keys().next() {
            return Err(format!("'{orphan}' is shipped but belongs to no group"));
        }
        Ok(Catalog { groups, katas })
    }

    pub fn kata(&self, id: &str) -> Option<&Kata> {
        self.katas.iter().find(|kata| kata.id == id)
    }

    pub fn max_points(&self) -> u32 {
        self.katas.iter().map(|kata| kata.points).sum()
    }

    /// Which groups are open for this progress: the first always, every later
    /// one once each kata of the group before it is passed.
    pub fn unlocked_groups(&self, progress: &HashMap<String, Progress>) -> Vec<bool> {
        let mut open = true;
        self.groups
            .iter()
            .map(|group| {
                let this_one = open;
                open = open
                    && group.katas.iter().all(|index| {
                        progress
                            .get(&self.katas[*index].id)
                            .is_some_and(|row| row.passed)
                    });
                this_one
            })
            .collect()
    }

    pub fn is_unlocked(&self, kata: &Kata, progress: &HashMap<String, Progress>) -> bool {
        self.unlocked_groups(progress)[kata.group]
    }

    pub fn info(
        &self,
        kata: &Kata,
        unlocked: bool,
        progress: &HashMap<String, Progress>,
    ) -> KataInfo {
        let row = progress.get(&kata.id);
        let status = match row {
            _ if !unlocked => STATUS_LOCKED,
            Some(row) if row.passed => STATUS_PASSED,
            Some(_) => STATUS_ATTEMPTED,
            None => STATUS_OPEN,
        };
        KataInfo {
            kata_id: kata.id.clone(),
            group_id: self.groups[kata.group].id.clone(),
            position: kata.position,
            points: kata.points,
            tier: kata.tier.clone(),
            titles: kata.titles.clone(),
            summaries: kata.summaries.clone(),
            status: status.to_string(),
            attempts: row.map_or(0, |row| row.attempts),
            best_score: row.and_then(|row| row.best_score),
            points_earned: row.map_or(0, |row| row.points),
        }
    }

    /// The course as one caller sees it.
    pub fn overview(&self, progress: &HashMap<String, Progress>) -> Overview {
        let unlocked = self.unlocked_groups(progress);
        let katas: Vec<KataInfo> = self
            .katas
            .iter()
            .map(|kata| self.info(kata, unlocked[kata.group], progress))
            .collect();
        let groups = self
            .groups
            .iter()
            .enumerate()
            .map(|(index, group)| KataGroupInfo {
                group_id: group.id.clone(),
                position: index as u32 + 1,
                titles: group.titles.clone(),
                kata_count: group.katas.len() as u32,
                passed_count: group
                    .katas
                    .iter()
                    .filter(|k| katas[**k].status == STATUS_PASSED)
                    .count() as u32,
                unlocked: unlocked[index],
            })
            .collect();
        Overview {
            passed_count: katas
                .iter()
                .filter(|kata| kata.status == STATUS_PASSED)
                .count() as u32,
            total_count: katas.len() as u32,
            points: katas.iter().map(|kata| kata.points_earned).sum(),
            max_points: self.max_points(),
            groups,
            katas,
        }
    }
}

pub struct Overview {
    pub groups: Vec<KataGroupInfo>,
    pub katas: Vec<KataInfo>,
    pub passed_count: u32,
    pub total_count: u32,
    pub points: u32,
    pub max_points: u32,
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

/// One person's row of one kata, as `kata_progress` stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub passed: bool,
    pub attempts: u32,
    pub best_score: Option<f64>,
    pub points: u32,
}

/// The number an attempt contributes to `best_score`, in [0, 1] with 1 the
/// ideal: a fidelity as it is, a distance as its complement.
fn score(grade: &KataGrade) -> Option<f64> {
    let value = grade.value?;
    Some(if grade.metric == METRIC_TVD {
        1.0 - value
    } else {
        value
    })
}

/// The row after one graded attempt, and the points that attempt added.
///
/// Points are awarded ONCE: on the first pass. A later pass raises nothing, and
/// a program the front end rejected is not an attempt at all — the caller does
/// not call this for it.
pub fn record(previous: Option<&Progress>, kata: &Kata, grade: &KataGrade) -> (Progress, u32) {
    let before = previous.cloned().unwrap_or(Progress {
        passed: false,
        attempts: 0,
        best_score: None,
        points: 0,
    });
    let passed_now = grade.outcome == OUTCOME_PASSED;
    let awarded = if passed_now && !before.passed {
        kata.points
    } else {
        0
    };
    let best_score = match (before.best_score, score(grade)) {
        (Some(old), Some(new)) => Some(old.max(new)),
        (old, new) => old.or(new),
    };
    (
        Progress {
            passed: before.passed || passed_now,
            attempts: before.attempts + 1,
            best_score,
            points: before.points + awarded,
        },
        awarded,
    )
}

/// One person's totals, the input of the ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct Standing {
    pub user_id: String,
    pub katas_passed: u32,
    pub points: u32,
}

/// The ranking of a laboratory: how many people are ranked, the top
/// [`KATA_RANKING_TOP`] and the caller's own place.
///
/// Competition ranking over (points, katas passed): people with the same pair
/// share a place and the next place skips, so nobody is placed above somebody
/// they are level with. Ties inside a place are ordered by user id only so the
/// list is stable between two requests.
pub fn ranking(
    mut standings: Vec<Standing>,
    caller: &str,
) -> (u32, Vec<(u32, Standing)>, Option<(u32, Standing)>) {
    standings.retain(|standing| standing.points > 0);
    standings.sort_by(|a, b| {
        b.points
            .cmp(&a.points)
            .then(b.katas_passed.cmp(&a.katas_passed))
            .then_with(|| a.user_id.cmp(&b.user_id))
    });
    let mut placed: Vec<(u32, Standing)> = Vec::with_capacity(standings.len());
    for (index, standing) in standings.into_iter().enumerate() {
        let position = match placed.last() {
            Some((position, last))
                if last.points == standing.points && last.katas_passed == standing.katas_passed =>
            {
                *position
            }
            _ => index as u32 + 1,
        };
        placed.push((position, standing));
    }
    let total = placed.len() as u32;
    let me = placed
        .iter()
        .find(|(_, standing)| standing.user_id == caller)
        .cloned();
    placed.truncate(KATA_RANKING_TOP);
    (total, placed, me)
}

// ---------------------------------------------------------------------------
// Grading
// ---------------------------------------------------------------------------

fn base_grade(kata: &Kata, got_qubits: usize) -> KataGrade {
    let (metric, threshold, expected_qubits) = match &kata.verify {
        Verify::State { qubits, .. } => (METRIC_FIDELITY, 1.0, *qubits),
        Verify::Unitary { qubits, .. } => (METRIC_PROCESS_FIDELITY, 1.0, *qubits),
        Verify::Counts { max_tvd, width, .. } => (METRIC_TVD, *max_tvd, *width),
    };
    KataGrade {
        outcome: OUTCOME_FAILED.to_string(),
        reason: String::new(),
        metric: metric.to_string(),
        value: None,
        threshold,
        expected_qubits: expected_qubits as u32,
        got_qubits: got_qubits as u32,
        shots: 0,
        counts: BTreeMap::new(),
        duration_ms: 0,
        diagnostic: None,
    }
}

fn refused(mut grade: KataGrade, reason: &str) -> KataGrade {
    grade.outcome = OUTCOME_FAILED.to_string();
    grade.reason = reason.to_string();
    grade
}

fn invalid(kata: &Kata, diagnostic: CircuitDiagnostic) -> KataGrade {
    let mut grade = base_grade(kata, 0);
    grade.outcome = OUTCOME_INVALID.to_string();
    grade.diagnostic = Some(diagnostic);
    grade
}

fn sim_options(seed: u64) -> SimOptions {
    SimOptions {
        precision: Precision::Double,
        max_qubits: KATA_MAX_QUBITS,
        seed,
    }
}

/// Process fidelity `|Tr(T†U)|² / d²` of two dense unitaries — 1 exactly when
/// they agree up to a global phase, and smoothly lower as they drift apart.
fn process_fidelity(target: &[Complex64], actual: &[Complex64], dim: usize) -> f64 {
    let trace: Complex64 = target.iter().zip(actual).map(|(t, u)| t.conj() * u).sum();
    trace.norm_sqr() / (dim * dim) as f64
}

/// Grades one OpenQASM 3 program against one kata.
pub fn grade(kata: &Kata, qasm3: &str) -> KataGrade {
    let started = Instant::now();
    let mut grade = match circuit::parse(qasm3, "") {
        Err(diagnostic) => invalid(kata, diagnostic),
        Ok(parsed) => grade_circuit(kata, &parsed.circuit),
    };
    grade.duration_ms = started.elapsed().as_millis() as u64;
    grade
}

fn grade_circuit(kata: &Kata, circuit: &Circuit) -> KataGrade {
    let got_qubits = circuit.num_qubits();
    let mut grade = base_grade(kata, got_qubits);
    let outcome = match &kata.verify {
        Verify::State {
            tolerance,
            target,
            qubits,
        } => {
            if got_qubits != *qubits {
                return refused(grade, "qubit_count");
            }
            if require_unitary(circuit).is_err() {
                return refused(grade, "not_unitary");
            }
            statevector(circuit, &sim_options(0), Device::Cpu, Cancel::none()).and_then(|state| {
                let fidelity = quantum_grade::state_fidelity(target, &state)?;
                let equal = quantum_grade::states_equal(target, &state, *tolerance)?;
                Ok((fidelity, equal))
            })
        }
        Verify::Unitary {
            tolerance,
            target,
            qubits,
        } => {
            if got_qubits != *qubits {
                return refused(grade, "qubit_count");
            }
            if require_unitary(circuit).is_err() {
                return refused(grade, "not_unitary");
            }
            circuit_unitary(circuit, &sim_options(0), Device::Cpu).and_then(|actual| {
                let fidelity = process_fidelity(target, &actual, 1 << qubits);
                let equal = quantum_grade::unitaries_equal(target, &actual, *tolerance)?;
                Ok((fidelity, equal))
            })
        }
        Verify::Counts {
            shots,
            seed,
            max_tvd,
            exact_support,
            expected,
            width,
        } => {
            if circuit.num_clbits() == 0 {
                return refused(grade, "no_measurement");
            }
            if circuit.num_clbits() != *width {
                return refused(grade, "clbit_count");
            }
            let sampled = run(
                circuit,
                &sim_options(*seed),
                Device::Cpu,
                *shots,
                Cancel::none(),
            );
            let result = match sampled {
                Ok(result) => result,
                Err(error) => return invalid(kata, circuit::diagnostic(&error)),
            };
            let ideal: BTreeMap<String, u64> = expected
                .iter()
                .map(|(bits, p)| (bits.clone(), (p * 1e9).round() as u64))
                .collect();
            let tvd = match quantum_grade::total_variation_distance(&result.counts, &ideal) {
                Ok(tvd) => tvd,
                Err(error) => return invalid(kata, circuit::diagnostic(&error)),
            };
            grade.value = Some(tvd);
            grade.shots = result.shots;
            let strays = *exact_support
                && result.counts.iter().any(|(bits, count)| {
                    *count > 0 && expected.get(bits).copied().unwrap_or(0.0) == 0.0
                });
            grade.counts = result.counts;
            return if tvd >= *max_tvd {
                refused(grade, "above_threshold")
            } else if strays {
                refused(grade, "unexpected_outcomes")
            } else {
                grade.outcome = OUTCOME_PASSED.to_string();
                grade
            };
        }
    };
    match outcome {
        Ok((fidelity, equal)) => {
            grade.value = Some(fidelity);
            if equal {
                grade.outcome = OUTCOME_PASSED.to_string();
                grade
            } else {
                refused(grade, "mismatch")
            }
        }
        Err(error) => invalid(kata, circuit::diagnostic(&error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kata(id: &str) -> &'static Kata {
        catalog().kata(id).expect("a shipped kata")
    }

    fn progress_of(passed: &[&str]) -> HashMap<String, Progress> {
        passed
            .iter()
            .map(|id| {
                (
                    id.to_string(),
                    Progress {
                        passed: true,
                        attempts: 1,
                        best_score: Some(1.0),
                        points: kata(id).points,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn the_catalog_ships_ten_katas_in_two_groups_with_the_order_of_the_plan() {
        let catalog = catalog();
        assert_eq!(catalog.katas.len(), 10);
        assert_eq!(catalog.groups.len(), 2);
        let ids: Vec<&str> = catalog.katas.iter().map(|k| k.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "the directory prefixes are the course order");
        for (index, kata) in catalog.katas.iter().enumerate() {
            assert_eq!(kata.position as usize, index + 1);
            assert!(kata.task.contains_key("pl") && kata.task.contains_key("en"));
        }
        assert_eq!(catalog.max_points(), 860);
    }

    /// The reference answers are the proof that each task can be solved with the
    /// gates it names, and the skeletons are the proof that an untouched editor
    /// is not a passing answer.
    #[test]
    fn every_shipped_solution_passes_and_every_skeleton_does_not() {
        for kata in &catalog().katas {
            let solved = grade(kata, &kata.solution);
            assert_eq!(
                solved.outcome, OUTCOME_PASSED,
                "{}: the solution must pass, got {solved:?}",
                kata.id
            );
            assert!(solved.reason.is_empty());

            let untouched = grade(kata, &kata.starter);
            assert_eq!(
                untouched.outcome, OUTCOME_FAILED,
                "{}: the skeleton must be parseable and must not pass, got {untouched:?}",
                kata.id
            );
            assert!(!untouched.reason.is_empty());
        }
    }

    #[test]
    fn a_verdict_comes_back_well_under_the_hundred_milliseconds_of_the_plan() {
        for kata in &catalog().katas {
            let solved = grade(kata, &kata.solution);
            assert!(
                solved.duration_ms < 100,
                "{} took {} ms",
                kata.id,
                solved.duration_ms
            );
        }
    }

    #[test]
    fn grading_is_deterministic_down_to_the_counts() {
        let bell = kata("10-bell-state");
        let first = grade(bell, &bell.solution);
        let second = grade(bell, &bell.solution);
        assert_eq!(first.counts, second.counts);
        assert_eq!(first.value, second.value);
        assert_eq!(first.shots, 1024);
        assert_eq!(first.counts.values().sum::<u64>(), 1024);
        assert!(first.value.expect("a distance") < 0.05);
    }

    #[test]
    fn a_state_equal_up_to_a_global_phase_passes() {
        // -|+> is |+> with a global phase of π: the same physical state.
        let h = kata("01-superposition-h");
        let program =
            "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nh q[0];\ngphase(pi);\n";
        let verdict = grade(h, program);
        assert_eq!(verdict.outcome, OUTCOME_PASSED, "{verdict:?}");
        assert!((verdict.value.expect("fidelity") - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_wrong_state_reports_its_fidelity() {
        // H then Z is |−>, which has fidelity 0 with |+>.
        let h = kata("01-superposition-h");
        let program = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nh q[0];\nz q[0];\n";
        let verdict = grade(h, program);
        assert_eq!(verdict.outcome, OUTCOME_FAILED);
        assert_eq!(verdict.reason, "mismatch");
        assert_eq!(verdict.metric, METRIC_FIDELITY);
        assert!(verdict.value.expect("fidelity") < 1e-12);
    }

    #[test]
    fn an_unequal_unitary_is_told_apart_by_its_process_fidelity() {
        // A lone T is not S: the matrices differ by a quarter-turn on |1>.
        let s = kata("05-phase-gates");
        let program = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nt q[0];\n";
        let verdict = grade(s, program);
        assert_eq!(verdict.reason, "mismatch");
        assert_eq!(verdict.metric, METRIC_PROCESS_FIDELITY);
        let value = verdict.value.expect("fidelity");
        assert!(value > 0.0 && value < 1.0, "{value}");
    }

    #[test]
    fn the_refusals_name_what_was_wrong_with_the_shape_of_the_program() {
        let h = kata("01-superposition-h");
        let two_qubits = "OPENQASM 3.0;\nqubit[2] q;\n";
        assert_eq!(grade(h, two_qubits).reason, "qubit_count");
        assert_eq!(grade(h, two_qubits).got_qubits, 2);

        let measured = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nbit[1] c;\nh q[0];\nc = measure q;\n";
        assert_eq!(grade(h, measured).reason, "not_unitary");

        let bell = kata("10-bell-state");
        let unmeasured =
            "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nh q[0];\ncx q[0], q[1];\n";
        assert_eq!(grade(bell, unmeasured).reason, "no_measurement");
        let narrow = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nbit[1] c;\nh q[0];\nc[0] = measure q[0];\n";
        assert_eq!(grade(bell, narrow).reason, "clbit_count");
    }

    #[test]
    fn a_distribution_with_a_stray_outcome_is_refused_even_inside_the_distance() {
        // Kata 06 expects {1: 1.0}. Flipping with a rotation that leaves 2 % in
        // 0 stays inside TVD 0.05 but breaks the exact-support rule.
        let x_basis = kata("06-x-basis-measurement");
        let leaky = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nbit[1] c;\nry(2.8) q[0];\nc = measure q;\n";
        let verdict = grade(x_basis, leaky);
        assert_eq!(verdict.outcome, OUTCOME_FAILED);
        assert!(
            verdict.reason == "unexpected_outcomes" || verdict.reason == "above_threshold",
            "{verdict:?}"
        );
        assert!(verdict.counts.contains_key("0"));
    }

    #[test]
    fn a_program_the_front_end_rejects_is_invalid_and_carries_the_diagnostic() {
        let h = kata("01-superposition-h");
        let verdict = grade(h, "OPENQASM 3.0;\nqubit[1] q;\nnot_a_gate q[0];\n");
        assert_eq!(verdict.outcome, OUTCOME_INVALID);
        let diagnostic = verdict.diagnostic.expect("a diagnostic");
        assert!(diagnostic.line.is_some(), "{diagnostic:?}");
    }

    #[test]
    fn a_register_over_the_ceiling_is_refused_before_it_is_allocated() {
        let bell = kata("10-bell-state");
        let huge = "OPENQASM 3.0;\nqubit[30] q;\nbit[2] c;\n";
        let verdict = grade(bell, huge);
        assert_eq!(verdict.outcome, OUTCOME_INVALID);
        assert_eq!(verdict.diagnostic.expect("a diagnostic").kind, "capacity");
    }

    #[test]
    fn groups_open_one_after_another() {
        let catalog = catalog();
        let none = HashMap::new();
        assert_eq!(catalog.unlocked_groups(&none), vec![true, false]);

        let almost = progress_of(&[
            "01-superposition-h",
            "02-x-gate",
            "03-rotations",
            "04-bloch-sphere",
            "05-phase-gates",
        ]);
        assert_eq!(catalog.unlocked_groups(&almost), vec![true, false]);

        let mut done = almost;
        done.extend(progress_of(&["06-x-basis-measurement"]));
        assert_eq!(catalog.unlocked_groups(&done), vec![true, true]);
        assert!(catalog.is_unlocked(kata("07-two-qubit-product"), &done));
        assert!(!catalog.is_unlocked(kata("07-two-qubit-product"), &none));
    }

    #[test]
    fn the_overview_reports_status_progress_and_points() {
        let catalog = catalog();
        let mut progress = progress_of(&["01-superposition-h", "02-x-gate"]);
        progress.insert(
            "03-rotations".to_string(),
            Progress {
                passed: false,
                attempts: 3,
                best_score: Some(0.9),
                points: 0,
            },
        );
        let overview = catalog.overview(&progress);
        let status = |id: &str| {
            overview
                .katas
                .iter()
                .find(|k| k.kata_id == id)
                .expect("kata")
                .status
                .clone()
        };
        assert_eq!(status("01-superposition-h"), STATUS_PASSED);
        assert_eq!(status("03-rotations"), STATUS_ATTEMPTED);
        assert_eq!(status("04-bloch-sphere"), STATUS_OPEN);
        assert_eq!(status("07-two-qubit-product"), STATUS_LOCKED);
        assert_eq!(overview.passed_count, 2);
        assert_eq!(overview.total_count, 10);
        assert_eq!(overview.points, 120);
        assert_eq!(overview.max_points, 860);
        assert_eq!(overview.groups[0].passed_count, 2);
        assert!(overview.groups[0].unlocked && !overview.groups[1].unlocked);
    }

    #[test]
    fn points_are_awarded_once_and_the_best_score_only_rises() {
        let bell = kata("10-bell-state");
        let mut grade = base_grade(bell, 2);
        grade.value = Some(0.5);
        let (first, awarded) = record(None, bell, &grade);
        assert_eq!(awarded, 0);
        assert!(!first.passed);
        assert_eq!(first.attempts, 1);
        assert_eq!(first.best_score, Some(0.5), "a distance of 0.5 scores 0.5");

        grade.outcome = OUTCOME_PASSED.to_string();
        grade.value = Some(0.02);
        let (second, awarded) = record(Some(&first), bell, &grade);
        assert_eq!(awarded, 120);
        assert!(second.passed);
        assert_eq!(second.points, 120);
        assert!((second.best_score.expect("score") - 0.98).abs() < 1e-12);

        // Passing it again earns nothing, and a worse pass cannot lower the best.
        grade.value = Some(0.04);
        let (third, awarded) = record(Some(&second), bell, &grade);
        assert_eq!(awarded, 0);
        assert_eq!(third.points, 120);
        assert_eq!(third.attempts, 3);
        assert!((third.best_score.expect("score") - 0.98).abs() < 1e-12);

        // A failure after a pass does not take the pass back.
        grade.outcome = OUTCOME_FAILED.to_string();
        let (fourth, _) = record(Some(&third), bell, &grade);
        assert!(fourth.passed);
    }

    fn standing(user: &str, passed: u32, points: u32) -> Standing {
        Standing {
            user_id: user.to_string(),
            katas_passed: passed,
            points,
        }
    }

    #[test]
    fn the_ranking_lists_the_top_five_and_the_callers_own_place() {
        let rows = vec![
            standing("a", 10, 860),
            standing("b", 9, 740),
            standing("c", 8, 640),
            standing("d", 7, 540),
            standing("e", 6, 440),
            standing("me", 2, 120),
            standing("zero", 0, 0),
        ];
        let (total, top, me) = ranking(rows, "me");
        assert_eq!(total, 6, "a person with no points is not ranked");
        assert_eq!(top.len(), KATA_RANKING_TOP);
        assert_eq!(top[0].1.user_id, "a");
        let (position, mine) = me.expect("the caller is ranked");
        assert_eq!(position, 6);
        assert_eq!(mine.points, 120);
    }

    #[test]
    fn people_with_the_same_score_share_a_place_and_the_next_one_skips() {
        let rows = vec![
            standing("a", 5, 400),
            standing("c", 5, 400),
            standing("b", 5, 400),
            standing("d", 4, 300),
        ];
        let (_, top, me) = ranking(rows, "nobody");
        let positions: Vec<u32> = top.iter().map(|(p, _)| *p).collect();
        assert_eq!(positions, vec![1, 1, 1, 4]);
        assert!(me.is_none(), "an unranked caller has no place");
        // The tie order is by id, so two requests list the same people.
        let ids: Vec<&str> = top.iter().map(|(_, s)| s.user_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn a_broken_catalog_is_described_not_accepted() {
        assert!(task_sections("x", "text before").is_err());
        let sections = task_sections("x", "@@ pl\njeden\n\n@@ en\none\n").expect("sections");
        assert_eq!(sections["pl"], "jeden");
        assert_eq!(sections["en"], "one");
        assert!(task_sections("x", "@@ pl\na\n@@ pl\nb\n").is_err());

        let bad_state =
            "kind = \"state_equals\"\ntolerance = 1e-9\ntarget = [[1.0, 0.0], [1.0, 0.0]]\n";
        assert!(verify_of("x", bad_state).unwrap_err().contains("norm"));
        let bad_counts = "kind = \"counts_tvd_below\"\nshots = 10\nseed = 1\nmax_tvd = 0.1\n[expected]\n\"0\" = 0.4\n";
        assert!(verify_of("x", bad_counts).unwrap_err().contains("sum"));
    }
}
