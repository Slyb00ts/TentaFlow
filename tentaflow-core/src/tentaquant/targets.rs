// ===== File: tentaquant/targets.rs — execution targets and the `auto` rule =====
//
// A target is where a run executes (plan §3.1): `browser` is tier T0, the
// wasm build of the same simulator inside the dashboard, and `core:<node_id>`
// is tier T1, this crate running natively on one node of the fleet. The tiers
// above — T2 (the `quantum-python` service), T3 (GPU) and T4 (a real QPU) —
// do not exist yet, and this module reports them as UNAVAILABLE with a reason
// rather than hiding them: a user whose circuit is too big for T1 has to be
// told that the tier that could take it is not installed, not left guessing.
//
// A remote node is LISTED — the fleet's laboratories are one laboratory, and a
// target that exists must be visible — but it is not offered yet, and the
// reason is on the row. A unary request is routed to it by `targetNodeId` and
// executes on the Core that receives it, so the run itself would work; its two
// companions do not. A run stream is not forwardable across the mesh — plan
// §11.3 gives the generalised `AppStreamOpen` relay its own step of phase F3,
// because it repoints Code Studio in the same change — so the live evolution of
// a remote run never reaches this dashboard; and an artifact URL is signed by the
// issuer of the node that minted it and served from the node the browser is
// connected to, so a download of a remote run's state is refused. Advertising
// the target as available would trade an honest "not yet" for a run whose live
// view and downloads silently fail.
//
// `resolve` is the `device="auto"` rule of plan §5.3, evaluated server-side so
// the UI can show "auto → T1 · node-a" BEFORE the run starts. It is one
// deterministic rule, the same in Core and in the SDK, and it never picks a
// QPU: T4 is always an explicit choice with a cost estimate in front of it.

use tentaflow_protocol::tentaquant::{LabSettings, TargetInfo, TargetUnavailable};

use super::circuit;

/// The Core ceiling as it actually holds: the laboratory's setting, never above
/// what the simulator will allocate. A lab configured higher than this build
/// can serve must be told the smaller number, or the `auto` rule would promise
/// a tier that then refuses the circuit.
fn core_ceiling(settings: &LabSettings) -> u32 {
    settings.max_qubits_core.min(circuit::MAX_CORE_QUBITS)
}

/// One node of the fleet, as the lab's instance status left it.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeCandidate {
    pub node_id: String,
    pub node_name: String,
    pub is_local: bool,
    pub online: bool,
    /// The platform's `__node_status/<node_id>` verdict for this instance
    /// ("ready" | "unsupported" | "init_error" | "unknown").
    pub instance_status: String,
}

impl NodeCandidate {
    fn ready(&self) -> bool {
        self.instance_status == "ready"
    }

    /// Why this node cannot take a run right now, or `None` when it can.
    ///
    /// "unknown" means the platform's reconcile has not written a status for
    /// this instance on that node yet. For a REMOTE node that is a refusal —
    /// nothing here can prove the laboratory works there. For the node
    /// ANSWERING the request it is not: reaching this code means the instance
    /// is enabled, its matrix admitted the caller and its database opened, all
    /// of which the caller just exercised. Refusing on a status row that has
    /// not been written yet would make a freshly installed laboratory unable
    /// to run on the very node that installed it.
    pub fn blocked_reason(&self) -> Option<String> {
        if !self.online {
            return Some("node offline".to_string());
        }
        if self.ready() || (self.is_local && self.instance_status == "unknown") {
            return None;
        }
        Some(format!(
            "laboratory not ready on this node ({})",
            self.instance_status
        ))
    }
}

/// The upper qubit count the browser tier is offered for, from plan §5.3 rule
/// 1. Above it the state vector plus the copies a JIT makes stop being
/// comfortable in a 32-bit address space, so `auto` stops offering T0 — the
/// user may still pick it explicitly, with the ceiling of `max_qubits_browser`.
const AUTO_BROWSER_QUBITS: u32 = 20;

/// Why a healthy node of the fleet is still not an execution target. The two
/// halves a run needs besides the execution itself — its live stream and its
/// artifact downloads — are node-local in this build (see the file header).
const REMOTE_NOT_YET: &str = "runs on another node cannot stream their evolution here yet \
     and their artifacts stay on that node, so this laboratory only executes on the node you \
     are connected to";

/// Tiers this build does not implement, with the reason the UI shows. Kept in
/// ONE place so the target list and the `auto` rule cannot disagree about what
/// exists.
pub fn missing_tiers() -> Vec<TargetUnavailable> {
    vec![
        TargetUnavailable {
            tier: "T2".to_string(),
            reason: "the `quantum-python` kernel service is not part of this build".to_string(),
        },
        TargetUnavailable {
            tier: "T3".to_string(),
            reason: "no GPU tier in this build: the simulator runs on the CPU".to_string(),
        },
        TargetUnavailable {
            tier: "T4".to_string(),
            reason: "no QPU provider is configured for this laboratory".to_string(),
        },
    ]
}

/// Every target the laboratory offers: the browser, then Core on each node.
pub fn list(settings: &LabSettings, nodes: &[NodeCandidate]) -> Vec<TargetInfo> {
    let mut targets = vec![TargetInfo {
        target: "browser".to_string(),
        tier: "T0".to_string(),
        node_id: None,
        node_name: "browser".to_string(),
        is_local: true,
        online: true,
        available: true,
        max_qubits: settings.max_qubits_browser,
        // wasm32 has no f64 SIMD path worth the size, and the WebGPU backend
        // of plan §6.3 will only ever offer single precision.
        precision: "single".to_string(),
        reason: None,
    }];
    for node in nodes {
        let reason = node
            .blocked_reason()
            .or_else(|| (!node.is_local).then(|| REMOTE_NOT_YET.to_string()));
        targets.push(TargetInfo {
            target: format!("core:{}", node.node_id),
            tier: "T1".to_string(),
            node_id: Some(node.node_id.clone()),
            node_name: node.node_name.clone(),
            is_local: node.is_local,
            online: node.online,
            available: reason.is_none(),
            max_qubits: core_ceiling(settings),
            precision: "double".to_string(),
            reason,
        });
    }
    targets
}

/// What the `auto` rule decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    /// `browser`, `core:<node_id>`, or empty when no tier can take the work.
    pub target: String,
    /// "T0" | "T1" | "none".
    pub tier: String,
    pub node_id: Option<String>,
    pub reason: String,
    pub unavailable: Vec<TargetUnavailable>,
}

/// The `device="auto"` rule of plan §5.3, in its order:
///
///   1. a circuit of at most 20 qubits called from the browser stays in the
///      browser (T0) — it is already there and nothing has to travel;
///   2. up to the laboratory's `max_qubits_core` it goes to T1 on the node the
///      request reached, which is where the notebook is;
///   3. above that, T3 on the node with the most free VRAM — not in this
///      build, so it is reported as unavailable;
///   4. a cell that needs a Python kernel is T2 — not in this build either;
///   5. never a QPU. T4 is always an explicit choice with a cost estimate.
///
/// Rule 4 is evaluated first because rules 1–3 are about a CIRCUIT: a Python
/// cell cannot be answered by them at all, and pretending otherwise would send
/// it to a tier that cannot execute it.
pub fn resolve(
    settings: &LabSettings,
    local: &NodeCandidate,
    num_qubits: u32,
    from_browser: bool,
    needs_kernel: bool,
    may_use_gpu: bool,
) -> Resolution {
    let missing = missing_tiers();
    let tier_reason = |tier: &str| -> String {
        missing
            .iter()
            .find(|entry| entry.tier == tier)
            .map(|entry| entry.reason.clone())
            .unwrap_or_else(|| format!("tier {tier} is unavailable"))
    };

    if needs_kernel {
        return Resolution {
            target: String::new(),
            tier: "none".to_string(),
            node_id: None,
            reason: format!(
                "a Python cell needs the kernel tier (T2): {}",
                tier_reason("T2")
            ),
            unavailable: missing,
        };
    }

    if from_browser
        && num_qubits <= AUTO_BROWSER_QUBITS
        && num_qubits <= settings.max_qubits_browser
    {
        return Resolution {
            target: "browser".to_string(),
            tier: "T0".to_string(),
            node_id: None,
            reason: format!(
                "{num_qubits} qubits fit the browser (up to {AUTO_BROWSER_QUBITS}), so nothing \
                 leaves this machine"
            ),
            unavailable: missing,
        };
    }

    if num_qubits <= core_ceiling(settings) {
        if let Some(blocked) = local.blocked_reason() {
            return Resolution {
                target: String::new(),
                tier: "none".to_string(),
                node_id: None,
                reason: format!("this node cannot run the laboratory: {blocked}"),
                unavailable: missing,
            };
        }
        return Resolution {
            target: format!("core:{}", local.node_id),
            tier: "T1".to_string(),
            node_id: Some(local.node_id.clone()),
            reason: format!(
                "{num_qubits} qubits fit Core on {} (up to {})",
                local.node_name,
                core_ceiling(settings)
            ),
            unavailable: missing,
        };
    }

    // Rule 3 — and the honest end of it. The GPU tier is what a circuit this
    // size would need, and holding `quant.run.gpu` does not conjure one.
    let reason = if may_use_gpu {
        format!(
            "{num_qubits} qubits exceed the Core ceiling ({}) and would need the GPU tier (T3): {}",
            core_ceiling(settings),
            tier_reason("T3")
        )
    } else {
        format!(
            "{num_qubits} qubits exceed the Core ceiling ({}); the GPU tier (T3) needs the \
             `quant.run.gpu` permission and is not available in this build either",
            core_ceiling(settings)
        )
    };
    Resolution {
        target: String::new(),
        tier: "none".to_string(),
        node_id: None,
        reason,
        unavailable: missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, local: bool, online: bool, status: &str) -> NodeCandidate {
        NodeCandidate {
            node_id: id.to_string(),
            node_name: format!("{id}-host"),
            is_local: local,
            online,
            instance_status: status.to_string(),
        }
    }

    #[test]
    fn the_list_offers_the_browser_and_one_core_target_per_node() {
        let settings = LabSettings::default();
        let nodes = vec![
            node("a", true, true, "ready"),
            node("b", false, false, "ready"),
        ];
        let targets = list(&settings, &nodes);
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].tier, "T0");
        assert_eq!(targets[0].max_qubits, settings.max_qubits_browser);
        assert_eq!(targets[1].target, "core:a");
        assert!(targets[1].available);
        // An offline node is listed and refused, not hidden: the UI has to be
        // able to say why a target the user picked yesterday is gone today.
        assert_eq!(targets[2].target, "core:b");
        assert!(!targets[2].available);
        assert_eq!(targets[2].reason.as_deref(), Some("node offline"));
    }

    /// A healthy remote node is listed and refused WITH the reason: this build
    /// cannot stream a remote run's evolution or serve its artifacts, so an
    /// "available" row would promise a run whose live view never arrives.
    #[test]
    fn a_healthy_remote_node_is_listed_but_not_offered_yet() {
        let targets = list(
            &LabSettings::default(),
            &[
                node("a", true, true, "ready"),
                node("b", false, true, "ready"),
            ],
        );
        assert!(targets[1].available, "the local node still runs");
        assert!(!targets[2].available);
        let reason = targets[2].reason.as_deref().expect("a reason, not silence");
        assert!(reason.contains("another node"), "{reason}");
    }

    #[test]
    fn a_node_that_never_initialised_the_lab_is_not_a_target() {
        let targets = list(
            &LabSettings::default(),
            &[node("a", true, true, "init_error")],
        );
        assert!(!targets[1].available);
        assert!(targets[1].reason.as_deref().unwrap().contains("init_error"));
    }

    /// A status the reconcile has not written yet blocks a REMOTE node and not
    /// the local one: only for the local one has the request itself proved the
    /// laboratory works.
    #[test]
    fn an_unwritten_status_blocks_a_remote_node_only() {
        let targets = list(
            &LabSettings::default(),
            &[
                node("a", true, true, "unknown"),
                node("b", false, true, "unknown"),
            ],
        );
        assert!(targets[1].available);
        assert!(!targets[2].available);
    }

    #[test]
    fn auto_keeps_a_small_circuit_in_the_browser() {
        let r = resolve(
            &LabSettings::default(),
            &node("a", true, true, "ready"),
            12,
            true,
            false,
            true,
        );
        assert_eq!(r.tier, "T0");
        assert_eq!(r.target, "browser");
    }

    #[test]
    fn auto_sends_a_mid_size_circuit_to_core_on_this_node() {
        let settings = LabSettings::default();
        // 24 qubits is over the browser rule but inside `max_qubits_core`.
        let r = resolve(
            &settings,
            &node("a", true, true, "ready"),
            24,
            true,
            false,
            true,
        );
        assert_eq!(r.tier, "T1");
        assert_eq!(r.target, "core:a");
        assert_eq!(r.node_id.as_deref(), Some("a"));

        // The same circuit from a caller that is not the browser: T0 is not an
        // option at all, so the rule starts at T1.
        let r = resolve(
            &settings,
            &node("a", true, true, "ready"),
            8,
            false,
            false,
            true,
        );
        assert_eq!(r.tier, "T1");
    }

    /// The refusal that matters: above the ceiling there is nowhere to go in
    /// this build, and the answer says so instead of naming a tier that would
    /// silently do nothing.
    #[test]
    fn auto_refuses_above_the_core_ceiling_and_names_the_missing_tier() {
        let settings = LabSettings::default();
        let r = resolve(
            &settings,
            &node("a", true, true, "ready"),
            31,
            true,
            false,
            true,
        );
        assert_eq!(r.tier, "none");
        assert!(r.target.is_empty());
        assert!(r.reason.contains("T3"));
        assert!(r.unavailable.iter().any(|u| u.tier == "T4"));

        let denied = resolve(
            &settings,
            &node("a", true, true, "ready"),
            31,
            true,
            false,
            false,
        );
        assert!(denied.reason.contains("quant.run.gpu"));
    }

    #[test]
    fn auto_never_answers_a_python_cell_with_a_circuit_tier() {
        let r = resolve(
            &LabSettings::default(),
            &node("a", true, true, "ready"),
            4,
            true,
            true,
            true,
        );
        assert_eq!(r.tier, "none");
        assert!(r.reason.contains("T2"));
    }

    #[test]
    fn auto_refuses_when_the_local_node_cannot_run_the_lab() {
        let r = resolve(
            &LabSettings::default(),
            &node("a", true, true, "init_error"),
            4,
            false,
            false,
            true,
        );
        assert_eq!(r.tier, "none");
        assert!(r.reason.contains("init_error"));
    }
}
