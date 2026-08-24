// =============================================================================
// File: gpu_topology.rs — host GPU interconnect topology and the NCCL_P2P_LEVEL
// auto-tuning decision shared by the native and docker multi-GPU deploy paths.
// =============================================================================

use std::collections::HashMap;
use std::sync::OnceLock;

use super::LogSink;

/// Interconnect class of one GPU pair, the exact `nvidia-smi topo -m` cell
/// (`NV#` collapsed to NVLink). The NCCL decision reduces it via `is_remote`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// `NV#` — NVLink; NCCL already picks the right transport, never override.
    NvLink,
    /// `PIX` — same PCIe switch.
    Pix,
    /// `PXB` — multiple PCIe bridges, no host bridge crossing.
    Pxb,
    /// `PHB` — same host bridge; NCCL's default P2P level still covers it.
    Phb,
    /// `NODE` — crosses PCIe host bridges within one NUMA node.
    Node,
    /// `SYS` — crosses the QPI/UPI link between NUMA nodes.
    Sys,
    Unknown,
}

impl Link {
    fn parse(cell: &str) -> Self {
        let c = cell.trim();
        if c.starts_with("NV") {
            Link::NvLink
        } else {
            match c {
                "PIX" => Link::Pix,
                "PXB" => Link::Pxb,
                "PHB" => Link::Phb,
                "NODE" => Link::Node,
                "SYS" => Link::Sys,
                _ => Link::Unknown,
            }
        }
    }

    /// Wire label shared with the mesh protocol (`MeshGpuLink.link`).
    pub fn as_str(self) -> &'static str {
        match self {
            Link::NvLink => "NVL",
            Link::Pix => "PIX",
            Link::Pxb => "PXB",
            Link::Phb => "PHB",
            Link::Node => "NODE",
            Link::Sys => "SYS",
            Link::Unknown => "UNKNOWN",
        }
    }

    /// `NODE` / `SYS`: NCCL disables P2P by default here even though the driver
    /// can route it — the only classes where `NCCL_P2P_LEVEL=SYS` helps.
    fn is_remote(self) -> bool {
        matches!(self, Link::Node | Link::Sys)
    }
}

/// Undirected pair links + driver P2P read status, keyed by `(min, max)` index.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuTopology {
    links: HashMap<(u32, u32), Link>,
    p2p_ok: HashMap<(u32, u32), bool>,
    gpu_indices: Vec<u32>,
}

impl GpuTopology {
    /// Builds a topology from the `nvidia-smi topo -m` and `nvidia-smi topo -p2p r`
    /// outputs. Only the GPU×GPU block of each matrix is read (NIC columns and the
    /// affinity trailer are ignored).
    pub fn from_nvidia_smi(topo_matrix: &str, p2p_matrix: &str) -> Self {
        let mut topo = GpuTopology::default();
        for (a, b, cell) in parse_matrix(topo_matrix) {
            topo.links.insert(pair_key(a, b), Link::parse(cell));
            if !topo.gpu_indices.contains(&a) {
                topo.gpu_indices.push(a);
            }
        }
        for (a, b, cell) in parse_matrix(p2p_matrix) {
            topo.p2p_ok
                .insert(pair_key(a, b), cell.trim().eq_ignore_ascii_case("OK"));
        }
        topo.gpu_indices.sort_unstable();
        topo
    }

    /// Every GPU index present in the topology matrix, ascending.
    pub fn gpu_indices(&self) -> &[u32] {
        &self.gpu_indices
    }

    fn link(&self, a: u32, b: u32) -> Link {
        self.links
            .get(&pair_key(a, b))
            .copied()
            .unwrap_or(Link::Unknown)
    }

    fn p2p_ok(&self, a: u32, b: u32) -> bool {
        self.p2p_ok.get(&pair_key(a, b)).copied().unwrap_or(false)
    }

    /// Every known pair `(a, b, link, p2p_ok)` with `a < b`, ordered by index.
    /// `p2p_ok` is `None` when the P2P matrix had no cell for the pair.
    pub fn pairs(&self) -> impl Iterator<Item = (u32, u32, Link, Option<bool>)> + '_ {
        let mut keys: Vec<(u32, u32)> = self.links.keys().copied().collect();
        keys.sort_unstable();
        keys.into_iter()
            .map(move |(a, b)| (a, b, self.link(a, b), self.p2p_ok.get(&(a, b)).copied()))
    }
}

fn pair_key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

/// Yields `(row_gpu, col_gpu, cell)` for the GPU×GPU block of an nvidia-smi
/// matrix. nvidia-smi underlines the header with ANSI SGR even when piped, and
/// column labels are tab-separated ("CPU Affinity" contains a space), so the
/// escape codes are stripped first and rows are split on tabs.
fn parse_matrix(raw: &str) -> Vec<(u32, u32, &str)> {
    let mut lines = raw.lines();
    let header = loop {
        match lines.next() {
            Some(l) if l.trim().is_empty() => continue,
            Some(l) => break l,
            None => return Vec::new(),
        }
    };
    let header_cols: Vec<String> = header
        .split('\t')
        .map(|c| strip_ansi(c).trim().to_string())
        .collect();
    // Column positions of GPUs relative to the first labelled column; the row's
    // leading label cell occupies that same position, so offsets line up.
    let mut gpu_cols: Vec<(usize, u32)> = Vec::new();
    for (i, col) in header_cols.iter().enumerate() {
        if let Some(idx) = gpu_label_index(col) {
            gpu_cols.push((i, idx));
        }
    }
    if gpu_cols.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in lines {
        let cells: Vec<&str> = line.split('\t').collect();
        let Some(first) = cells.first() else { continue };
        let Some(row_gpu) = gpu_label_index(strip_ansi(first).trim()) else {
            // Legend / NIC rows end the GPU block.
            continue;
        };
        for (col, col_gpu) in &gpu_cols {
            if *col_gpu == row_gpu {
                continue;
            }
            if let Some(cell) = cells.get(*col) {
                out.push((row_gpu, *col_gpu, *cell));
            }
        }
    }
    out
}

fn gpu_label_index(label: &str) -> Option<u32> {
    label.strip_prefix("GPU")?.trim().parse().ok()
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Which GPUs a deploy will span, as seen by the engine process/container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuScope {
    /// Every GPU on the host (wizard mode `all`, docker `--gpus all`).
    All,
    /// Explicit host GPU indices (`gpu_ids` from the wizard, docker device ids).
    Indices(Vec<u32>),
}

/// Outcome of the pure decision, so callers can log the reasoning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Export `NCCL_P2P_LEVEL=SYS`; `pair` is the first remote-linked P2P-capable pair.
    Set { pair: (u32, u32), link: Link },
    /// Leave NCCL's default; the string is the human-readable reason.
    Keep(&'static str),
}

impl Decision {
    pub fn env_value(&self) -> Option<&'static str> {
        match self {
            Decision::Set { .. } => Some("SYS"),
            Decision::Keep(_) => None,
        }
    }
}

/// Decides whether `NCCL_P2P_LEVEL=SYS` helps for the GPUs in `gpu_ids`.
/// NCCL's default P2P level stops at PHB, so on multi-socket hosts a pair reached
/// over NODE/SYS silently falls back to host-staged copies even when the driver
/// reports P2P as available; raising the level restores direct transfers there.
/// NVLink anywhere in the set means NCCL already has the best path — do nothing.
pub fn nccl_p2p_level_for(topo: &GpuTopology, gpu_ids: &[u32]) -> Decision {
    let mut ids: Vec<u32> = gpu_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() < 2 {
        return Decision::Keep("single GPU");
    }
    let mut candidate: Option<(u32, u32)> = None;
    for (i, &a) in ids.iter().enumerate() {
        for &b in &ids[i + 1..] {
            match topo.link(a, b) {
                Link::NvLink => return Decision::Keep("NVLink present"),
                Link::Unknown => return Decision::Keep("unknown link class"),
                link if link.is_remote() => {
                    if !topo.p2p_ok(a, b) {
                        return Decision::Keep("P2P unavailable on a NODE/SYS pair");
                    }
                    candidate.get_or_insert((a, b));
                }
                _ => {}
            }
        }
    }
    match candidate {
        Some(pair) => Decision::Set {
            pair,
            link: topo.link(pair.0, pair.1),
        },
        None => Decision::Keep("all pairs within PHB"),
    }
}

/// Engines whose multi-GPU path runs on NCCL. llama.cpp/embedded/MLX split
/// tensors themselves; a Metal-served vLLM has no NCCL either.
pub fn engine_uses_nccl(engine_id: &str) -> bool {
    use crate::deploy::launch_dialect::{dialect_for, Dialect};
    let id = engine_id.to_ascii_lowercase();
    if id.ends_with("-metal") {
        return false;
    }
    matches!(dialect_for(&id), Dialect::Vllm | Dialect::Sglang)
        || id.starts_with("vllm")
        || id.starts_with("trt-llm")
        || id.starts_with("tensorrt-llm")
}

/// Host topology probed once per process — PCIe/NVLink wiring does not change
/// at runtime. `None` when `nvidia-smi` is missing or the matrix has no GPUs.
pub fn host_topology() -> Option<&'static GpuTopology> {
    static TOPO: OnceLock<Option<GpuTopology>> = OnceLock::new();
    TOPO.get_or_init(|| {
        let run = |args: &[&str]| {
            std::process::Command::new("nvidia-smi")
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        };
        let matrix = run(&["topo", "-m"])?;
        let p2p = run(&["topo", "-p2p", "r"])?;
        let topo = GpuTopology::from_nvidia_smi(&matrix, &p2p);
        (!topo.gpu_indices.is_empty()).then_some(topo)
    })
    .as_ref()
}

const NCCL_P2P_LEVEL: &str = "NCCL_P2P_LEVEL";

/// Applies the auto decision to a deploy env. Runs AFTER `apply_engine_env`: a
/// user-supplied `engine_env.NCCL_P2P_LEVEL` (any value, including empty) turns
/// the automation off, and an empty value is dropped from the env instead of
/// being exported as an empty string (which NCCL would treat as an invalid level).
pub fn apply_nccl_p2p_level_env(
    engine_id: &str,
    user_config: &serde_json::Value,
    scope: GpuScope,
    topo: Option<&GpuTopology>,
    env: &mut HashMap<String, String>,
    log_sink: Option<&LogSink>,
) {
    let explicit = user_config
        .get("engine_env")
        .and_then(|v| v.as_object())
        .and_then(|o| o.get(NCCL_P2P_LEVEL));
    if let Some(v) = explicit {
        if v.as_str().is_some_and(|s| s.trim().is_empty()) {
            env.remove(NCCL_P2P_LEVEL);
        }
        return;
    }
    if !engine_uses_nccl(engine_id) || env.contains_key(NCCL_P2P_LEVEL) {
        return;
    }
    let Some(topo) = topo else { return };
    let ids: Vec<u32> = match scope {
        GpuScope::All => topo.gpu_indices().to_vec(),
        GpuScope::Indices(ids) => ids,
    };
    let decision = nccl_p2p_level_for(topo, &ids);
    let msg = match &decision {
        Decision::Set { pair, link } => {
            env.insert(NCCL_P2P_LEVEL.to_string(), "SYS".to_string());
            format!(
                "[gpu] NCCL_P2P_LEVEL=SYS: GPU{} <-> GPU{} linked via {} with P2P available (gpus=[{}])",
                pair.0,
                pair.1,
                link.as_str(),
                join_ids(&ids)
            )
        }
        Decision::Keep(reason) => format!(
            "[gpu] NCCL_P2P_LEVEL left at NCCL default: {} (gpus=[{}])",
            reason,
            join_ids(&ids)
        ),
    };
    tracing::info!("{}", msg);
    if let Some(s) = log_sink {
        s.info(&msg);
    }
}

fn join_ids(ids: &[u32]) -> String {
    ids.iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Parses wizard `gpu_ids` (numbers or numeric strings) into host indices.
/// Non-numeric entries are dropped — the topology is keyed by nvidia-smi index.
pub fn parse_gpu_indices<'a>(ids: impl IntoIterator<Item = &'a str>) -> Vec<u32> {
    ids.into_iter()
        .filter_map(|s| s.trim().parse::<u32>().ok())
        .collect()
}

/// GPU scope of a native deploy from the wizard's `gpu_select_mode` / `gpu_ids`,
/// mirroring `apply_gpu_selection_env`: `none` → no GPUs, an empty `specific`
/// list behaves like `all`.
pub fn wizard_gpu_scope(user_config: &serde_json::Value) -> Option<GpuScope> {
    match user_config
        .get("gpu_select_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("all")
    {
        "none" => None,
        "specific" => {
            let ids: Vec<String> = user_config
                .get("gpu_ids")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|id| match id {
                            serde_json::Value::String(s) => Some(s.clone()),
                            serde_json::Value::Number(n) => Some(n.to_string()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let parsed = parse_gpu_indices(ids.iter().map(String::as_str));
            if parsed.is_empty() {
                Some(GpuScope::All)
            } else {
                Some(GpuScope::Indices(parsed))
            }
        }
        _ => Some(GpuScope::All),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topo(links: &[(u32, u32, &str)], p2p: &[(u32, u32, bool)]) -> GpuTopology {
        let mut t = GpuTopology::default();
        for (a, b, cell) in links {
            t.links.insert(pair_key(*a, *b), Link::parse(cell));
            for g in [a, b] {
                if !t.gpu_indices.contains(g) {
                    t.gpu_indices.push(*g);
                }
            }
        }
        for (a, b, ok) in p2p {
            t.p2p_ok.insert(pair_key(*a, *b), *ok);
        }
        t.gpu_indices.sort_unstable();
        t
    }

    const HAZAI_TOPO: &str =
        "\t\x1b[4mGPU0\tGPU1\tGPU2\tCPU Affinity\tNUMA Affinity\tGPU NUMA ID\x1b[0m\n\
GPU0\t X \tPHB\tNODE\t0-31\t0\t\tN/A\n\
GPU1\tPHB\t X \tNODE\t0-31\t0\t\tN/A\n\
GPU2\tNODE\tNODE\t X \t0-31\t0\t\tN/A\n\
\n\
Legend:\n\
\n\
  X    = Self\n";

    const HAZAI_P2P: &str = " \t\x1b[4mGPU0\tGPU1\tGPU2\t\x1b[0m\n \
GPU0\tX\tOK\tOK\t\n \
GPU1\tOK\tX\tOK\t\n \
GPU2\tOK\tOK\tX\t\n\
\n\
Legend:\n";

    #[test]
    fn parses_real_nvidia_smi_output() {
        let t = GpuTopology::from_nvidia_smi(HAZAI_TOPO, HAZAI_P2P);
        assert_eq!(t.gpu_indices(), &[0, 1, 2]);
        assert_eq!(t.link(0, 1), Link::Phb);
        assert_eq!(t.link(1, 0), Link::Phb);
        assert_eq!(t.link(0, 2), Link::Node);
        assert!(t.p2p_ok(0, 2));
        assert!(t.p2p_ok(2, 1));
        let pairs: Vec<_> = t.pairs().collect();
        assert_eq!(
            pairs,
            vec![
                (0, 1, Link::Phb, Some(true)),
                (0, 2, Link::Node, Some(true)),
                (1, 2, Link::Node, Some(true)),
            ]
        );
    }

    #[test]
    fn link_labels_and_pairs_without_p2p_matrix() {
        for (cell, label) in [
            ("NV1", "NVL"),
            ("NV18", "NVL"),
            ("PIX", "PIX"),
            ("PXB", "PXB"),
            ("PHB", "PHB"),
            ("NODE", "NODE"),
            ("SYS", "SYS"),
            ("??", "UNKNOWN"),
        ] {
            assert_eq!(Link::parse(cell).as_str(), label, "{cell}");
        }
        let t = topo(&[(1, 0, "PIX")], &[]);
        assert_eq!(t.pairs().collect::<Vec<_>>(), vec![(0, 1, Link::Pix, None)]);
    }

    #[test]
    fn single_gpu_keeps_default() {
        let t = topo(&[(0, 1, "NODE")], &[(0, 1, true)]);
        assert_eq!(nccl_p2p_level_for(&t, &[0]), Decision::Keep("single GPU"));
        assert_eq!(
            nccl_p2p_level_for(&t, &[1, 1]),
            Decision::Keep("single GPU")
        );
    }

    #[test]
    fn phb_only_keeps_default() {
        let t = topo(&[(0, 1, "PHB"), (0, 2, "PIX"), (1, 2, "PXB")], &[]);
        assert_eq!(
            nccl_p2p_level_for(&t, &[0, 1, 2]),
            Decision::Keep("all pairs within PHB")
        );
    }

    #[test]
    fn node_pair_with_p2p_sets_sys() {
        let t = topo(
            &[(0, 1, "PHB"), (0, 2, "NODE"), (1, 2, "NODE")],
            &[(0, 1, true), (0, 2, true), (1, 2, true)],
        );
        assert_eq!(
            nccl_p2p_level_for(&t, &[0, 1, 2]),
            Decision::Set {
                pair: (0, 2),
                link: Link::Node
            }
        );
        assert_eq!(
            nccl_p2p_level_for(&t, &[0, 1]),
            Decision::Keep("all pairs within PHB")
        );
        assert_eq!(nccl_p2p_level_for(&t, &[2, 0]).env_value(), Some("SYS"));
    }

    #[test]
    fn sys_pair_with_p2p_sets_sys() {
        let t = topo(&[(0, 3, "SYS")], &[(0, 3, true)]);
        assert_eq!(nccl_p2p_level_for(&t, &[0, 3]).env_value(), Some("SYS"));
    }

    #[test]
    fn nvlink_anywhere_keeps_default() {
        let t = topo(
            &[(0, 1, "NV4"), (0, 2, "NODE"), (1, 2, "NODE")],
            &[(0, 2, true), (1, 2, true)],
        );
        assert_eq!(
            nccl_p2p_level_for(&t, &[0, 1, 2]),
            Decision::Keep("NVLink present")
        );
    }

    #[test]
    fn remote_pair_without_p2p_keeps_default() {
        let t = topo(
            &[(0, 1, "NODE"), (1, 2, "NODE"), (0, 2, "PHB")],
            &[(0, 1, true), (1, 2, false)],
        );
        assert_eq!(
            nccl_p2p_level_for(&t, &[0, 1, 2]),
            Decision::Keep("P2P unavailable on a NODE/SYS pair")
        );
        let missing = topo(&[(0, 1, "SYS")], &[]);
        assert_eq!(missing.link(0, 1), Link::Sys);
        assert!(nccl_p2p_level_for(&missing, &[0, 1]).env_value().is_none());
    }

    #[test]
    fn unknown_link_keeps_default() {
        let t = topo(&[(0, 1, "NODE")], &[(0, 1, true)]);
        assert_eq!(
            nccl_p2p_level_for(&t, &[0, 5]),
            Decision::Keep("unknown link class")
        );
    }

    #[test]
    fn engine_gate() {
        for id in [
            "vllm",
            "vllm-spark",
            "vllm-dspark-src",
            "sglang",
            "qwen3-vl",
            "granite-4-1",
        ] {
            assert!(engine_uses_nccl(id), "{id}");
        }
        for id in ["llama-cpp", "vllm-metal", "mlx", "whisper", "searxng"] {
            assert!(!engine_uses_nccl(id), "{id}");
        }
    }

    fn remote_topo() -> GpuTopology {
        topo(&[(0, 1, "NODE")], &[(0, 1, true)])
    }

    #[test]
    fn apply_sets_env_for_all_and_specific_scopes() {
        let t = remote_topo();
        let cfg = serde_json::json!({});
        let mut env = HashMap::new();
        apply_nccl_p2p_level_env("vllm", &cfg, GpuScope::All, Some(&t), &mut env, None);
        assert_eq!(env.get(NCCL_P2P_LEVEL).map(String::as_str), Some("SYS"));

        let mut env = HashMap::new();
        apply_nccl_p2p_level_env(
            "sglang",
            &cfg,
            GpuScope::Indices(vec![0, 1]),
            Some(&t),
            &mut env,
            None,
        );
        assert_eq!(env.get(NCCL_P2P_LEVEL).map(String::as_str), Some("SYS"));

        let mut env = HashMap::new();
        apply_nccl_p2p_level_env(
            "vllm",
            &cfg,
            GpuScope::Indices(vec![1]),
            Some(&t),
            &mut env,
            None,
        );
        assert!(!env.contains_key(NCCL_P2P_LEVEL));

        let mut env = HashMap::new();
        apply_nccl_p2p_level_env("llama-cpp", &cfg, GpuScope::All, Some(&t), &mut env, None);
        assert!(!env.contains_key(NCCL_P2P_LEVEL));

        let mut env = HashMap::new();
        apply_nccl_p2p_level_env("vllm", &cfg, GpuScope::All, None, &mut env, None);
        assert!(!env.contains_key(NCCL_P2P_LEVEL));
    }

    #[test]
    fn explicit_engine_env_wins() {
        let t = remote_topo();
        let cfg = serde_json::json!({"engine_env": {"NCCL_P2P_LEVEL": "PXB"}});
        // Mirrors the real order: apply_engine_env already copied the user value.
        let mut env = HashMap::from([(NCCL_P2P_LEVEL.to_string(), "PXB".to_string())]);
        apply_nccl_p2p_level_env("vllm", &cfg, GpuScope::All, Some(&t), &mut env, None);
        assert_eq!(env.get(NCCL_P2P_LEVEL).map(String::as_str), Some("PXB"));
    }

    #[test]
    fn empty_engine_env_value_disables_and_is_not_exported() {
        let t = remote_topo();
        let cfg = serde_json::json!({"engine_env": {"NCCL_P2P_LEVEL": ""}});
        let mut env = HashMap::from([(NCCL_P2P_LEVEL.to_string(), String::new())]);
        apply_nccl_p2p_level_env("vllm", &cfg, GpuScope::All, Some(&t), &mut env, None);
        assert!(!env.contains_key(NCCL_P2P_LEVEL));

        let cfg = serde_json::json!({"engine_env": {"NCCL_P2P_LEVEL": "  "}});
        let mut env = HashMap::from([(NCCL_P2P_LEVEL.to_string(), "  ".to_string())]);
        apply_nccl_p2p_level_env("vllm", &cfg, GpuScope::All, Some(&t), &mut env, None);
        assert!(!env.contains_key(NCCL_P2P_LEVEL));
    }

    #[test]
    fn parse_gpu_indices_drops_non_numeric() {
        assert_eq!(parse_gpu_indices(["0", " 2 ", "GPU-uuid", ""]), vec![0, 2]);
    }

    #[test]
    fn wizard_scope_mirrors_gpu_selection() {
        assert_eq!(
            wizard_gpu_scope(&serde_json::json!({})),
            Some(GpuScope::All)
        );
        assert_eq!(
            wizard_gpu_scope(&serde_json::json!({"gpu_select_mode": "none"})),
            None
        );
        assert_eq!(
            wizard_gpu_scope(
                &serde_json::json!({"gpu_select_mode": "specific", "gpu_ids": ["1", 3]})
            ),
            Some(GpuScope::Indices(vec![1, 3]))
        );
        assert_eq!(
            wizard_gpu_scope(&serde_json::json!({"gpu_select_mode": "specific", "gpu_ids": []})),
            Some(GpuScope::All)
        );
    }
}
