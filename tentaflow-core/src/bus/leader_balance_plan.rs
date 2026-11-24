// =============================================================================
// File: bus/leader_balance_plan.rs — the pure planner behind "spread the
//       leaders" (TB-04 K5)
// =============================================================================
//
// Which topics should change leader so that the nodes lead about the same
// number of partitions, with as few moves as it takes. No I/O and no clock:
// the facts come in as `PlanInput`, the moves go out as `LeaderPlan`, the
// same input giving the same plan byte for byte (everything is ordered by
// name, never by hash or arrival). `leader_balance.rs` gathers the facts,
// checks the plan against the leaders' live state and executes it.
//
// UNIT OF MOVE. A topic and its `__dlq.<topic>` topic move as one unit
// (TB-04 P1, §10): `BusService::open_consumer` and everything built on it
// need one node to lead the whole topic, and the dead-letter retry needs the
// DLQ partition led where its source partition is.
//
// WHAT "LED" MEANS. A node leads a partition when the assignment row names it
// `leader_node_id`, that is, the CURRENT leader: the node that carries the
// writes now, which is what is being equalized. This differs from the load
// `BusService::create_topic` balances (`leader_counts`, the PREFERRED leader,
// `replicas[0]`): placement is about intent and must not be skewed by a
// failover in progress, while a rebalance acts on what is happening. The two
// meet again once a plan has run: every move that is not a return to the
// preferred node makes the target the preferred node, and a return changes
// nothing, so the units a plan touches end up led by their preferred node.
//
// ORDER. First every unit that is not led by its preferred node returns to it
// (a node that came back after a failure takes its topics back), then the
// remaining imbalance is equalized by moving whole units from the most to the
// least loaded node. A unit moves at most once per plan.
//
// A return that is blocked only for a reason that may pass (the preferred
// node does not answer or is behind, or is outside the in-sync set) is not
// replaced by a move elsewhere: the unit counts as led by its preferred node
// while the rest is equalized. A move to another node would make that node
// the preferred one for good and fight the placement of K4 and the return of
// K6. A return blocked for a lasting reason (a paused group, a node that
// cannot take forwarded transfers, no right) leaves the unit where it is,
// counted where it is.
//
// THE SAME LINE IS DRAWN BY THE AUTOMATIC RETURN (`leader_return`, K6), which
// is what makes counting on a transient return honest: it returns a topic
// once its preferred node has been in sync for a while, so a return that is
// only waiting for the node (outside the in-sync set, behind, not answering,
// forwarding not known yet) does happen by itself; and it never returns a
// topic whose group is paused or whose preferred node cannot take forwarded
// requests, which are exactly the lasting reasons (`is_lasting`). The one
// case where nothing returns by itself is the switch being off on the node
// that makes the plan: `PlanInput::automatic_return_off` then counts every
// unit where it is. (The switch is per node, and it is the leading node's
// that counts, so with nodes that disagree this is the plan's best guess.)
//
// GOAL. Among the nodes that answer, the plan keeps moving a whole unit from
// the most to the least loaded node for as long as one move makes their gap
// smaller. Each move is the one that leaves the smallest gap between the two
// nodes, so the number of moves stays small; it is a greedy choice, not a
// proven minimum. The counts therefore end up differing by at most the weight
// of the smallest movable unit only when the weights allow it (units of 8, 3
// and 1 over twelve partitions cannot be split better than 4 apart). When
// the gap that is left is wider than that and no refusal explains it, the
// plan names the units that would not improve it (`would_not_improve`).
//
// WHAT THE PLAN SAYS ABOUT LOAD. `per_node` counts the partitions of the
// organisation the plan is for (all of them for a plan with no organisation,
// the one the system makes after an upgrade); the partitions of other
// organisations weigh on the balance but are not reported.

use std::collections::{BTreeMap, BTreeSet};

use crate::bus::dlq::DLQ_TOPIC_PREFIX;

/// Why a topic cannot move (the stable machine values of
/// `LeaderBalanceBlockerWire::reason`).
pub mod reason {
    /// The node that leads the topic, or the target, does not answer.
    pub const NODE_UNREACHABLE: &str = "node_unreachable";
    /// The leader or the target does not understand forwarded transfers (a
    /// build from before them).
    pub const NODE_WITHOUT_FORWARDING: &str = "node_without_forwarding";
    /// The target is no replica of some partition of the unit, or is outside
    /// its in-sync set.
    pub const TARGET_OUTSIDE_ISR: &str = "target_outside_isr";
    /// The target is in the in-sync set but has not caught up with the leader
    /// and would not within the hold a transfer allows itself.
    pub const TARGET_BEHIND: &str = "target_behind";
    /// A consumer group of the topic is paused: the pause is not replicated,
    /// so the new leader would resume it.
    pub const CONSUMER_GROUP_PAUSED: &str = "consumer_group_paused";
    /// The caller has no admin right on the topic.
    pub const NO_ADMIN_RIGHT: &str = "no_admin_right";
    /// The leader of some partition could not be asked, or does not lead it
    /// any more.
    pub const LEADER_UNAVAILABLE: &str = "leader_unavailable";
    /// Moving this topic would not make the counts any more even.
    pub const WOULD_NOT_IMPROVE: &str = "would_not_improve";
}

/// One partition row of an instance, as the planner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionFacts {
    pub org_id: String,
    pub topic: String,
    pub partition: u32,
    /// The current leader (`leader_node_id` of the row).
    pub leader: String,
    /// The replica set; position 0 is the preferred leader.
    pub replicas: Vec<String>,
    /// The in-sync set the planner may rely on.
    pub isr: Vec<String>,
    /// `isr` is the live set of the leader that holds it; `false` for the
    /// ledger's last materialized set, which only changes at elections and
    /// reconfigurations and can lag the truth.
    pub isr_live: bool,
}

/// The facts a plan is made from.
#[derive(Debug, Clone, Default)]
pub struct PlanInput {
    /// Every partition row of the instance, of every organisation: all of it
    /// counts as load, only `scope_org`/`scope_topic` of it may move.
    pub partitions: Vec<PartitionFacts>,
    /// Nodes that answer, this one included.
    pub reachable: BTreeSet<String>,
    /// Nodes that understand forwarded transfers, this one included.
    pub forward_capable: BTreeSet<String>,
    /// Topics (organisation, name) with a paused consumer group. A DLQ name
    /// counts for the unit it belongs to.
    pub paused: BTreeSet<(String, String)>,
    /// Units (organisation, source topic) the leaders' own checks refused,
    /// with the reason and the node concerned.
    pub vetoed: BTreeMap<(String, String), (String, Option<String>)>,
    /// Only topics of this organisation may move.
    pub scope_org: Option<String>,
    /// Only this topic (and its DLQ) may move.
    pub scope_topic: Option<String>,
    /// The automatic return of topics to their preferred leaders is off, so
    /// a return blocked for now is not expected to happen by itself and the
    /// unit counts where it is, as for a lasting reason.
    pub automatic_return_off: bool,
}

/// One move of a plan: every partition of the topic and its DLQ topic that is
/// not led by `to` goes to `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedMove {
    pub org_id: String,
    pub topic: String,
    /// The partitions of `topic` (the DLQ topic's partitions go with them).
    pub partitions: Vec<u32>,
    /// The node that leads most of the unit now. Equal to `to` for a unit
    /// whose strays are gathered on the node that already leads most of it.
    pub from: String,
    pub to: String,
    /// `to` is the preferred node of the unit: a return, which leaves
    /// `replicas[0]` as it is.
    pub returns_to_preferred: bool,
    /// Some partition of the unit was judged by the ledger's ISR rather than
    /// a live one.
    pub isr_uncertain: bool,
}

/// A unit the plan wanted to move and could not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanBlocker {
    pub org_id: String,
    pub topic: String,
    pub reason: String,
    pub node_id: Option<String>,
}

/// Led partitions of one node before and after the plan, of the organisation
/// the plan is for (of every organisation for a plan with none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeLoad {
    pub node_id: String,
    pub before: u32,
    pub after: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeaderPlan {
    pub moves: Vec<PlannedMove>,
    pub per_node: Vec<NodeLoad>,
    pub blockers: Vec<PlanBlocker>,
}

/// The topic a (possibly DLQ) topic name belongs to.
fn unit_topic(topic: &str) -> &str {
    topic.strip_prefix(DLQ_TOPIC_PREFIX).unwrap_or(topic)
}

/// `true` for a topic the broker keeps for itself (`__bus.metrics`): it stays
/// where it was created.
pub(super) fn is_internal(unit: &str) -> bool {
    unit.starts_with("__")
}

struct Unit {
    org: String,
    topic: String,
    /// Partitions of the source topic.
    source_partitions: Vec<u32>,
    /// Rows led per node.
    led: BTreeMap<String, u32>,
    weight: u32,
    home: String,
    preferred: Option<String>,
    /// Every partition row of the unit: its replicas and ISR.
    rows: Vec<RowFacts>,
    movable: bool,
    moved: bool,
}

struct RowFacts {
    replicas: Vec<String>,
    isr: Vec<String>,
    isr_live: bool,
}

impl Unit {
    fn key(&self) -> (String, String) {
        (self.org.clone(), self.topic.clone())
    }

    /// Led by the preferred node throughout.
    fn is_home_at_preferred(&self) -> bool {
        self.preferred
            .as_ref()
            .is_some_and(|p| self.led.len() == 1 && self.led.contains_key(p))
    }
}

/// The node most partitions of `led` are led by; the lowest id on a tie.
pub(super) fn majority(led: &BTreeMap<String, u32>) -> String {
    led.iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        .map(|(node, _)| node.clone())
        .unwrap_or_default()
}

/// The preferred node of a unit: the node most of its rows name first
/// (`firsts`, one entry per row, empty names counted as none). A tie goes to
/// the node that leads most of the unit (`led`), then to the lowest id, so a
/// unit whose rows disagree (a transfer that raced an election left some
/// rows behind) is still read one way, by the planner and by the automatic
/// return alike.
pub(super) fn plurality_preferred<'a>(
    firsts: impl IntoIterator<Item = &'a str>,
    led: &BTreeMap<String, u32>,
) -> Option<String> {
    let mut votes: BTreeMap<&str, u32> = BTreeMap::new();
    for first in firsts.into_iter().filter(|f| !f.is_empty()) {
        *votes.entry(first).or_insert(0) += 1;
    }
    votes
        .into_iter()
        .max_by(|a, b| {
            let led_of = |node: &str| led.get(node).copied().unwrap_or(0);
            a.1.cmp(&b.1)
                .then(led_of(a.0).cmp(&led_of(b.0)))
                .then(b.0.cmp(a.0))
        })
        .map(|(node, _)| node.to_string())
}

/// Makes the plan for `input`. See the module doc for what it aims at.
pub fn plan(input: &PlanInput) -> LeaderPlan {
    let mut units = build_units(input);
    let mut load: BTreeMap<String, u32> = BTreeMap::new();
    for p in &input.partitions {
        *load.entry(p.leader.clone()).or_insert(0) += 1;
    }
    let in_view = |org: &str| input.scope_org.as_ref().is_none_or(|o| o == org);
    let mut shown: BTreeMap<String, u32> = BTreeMap::new();
    for p in input.partitions.iter().filter(|p| in_view(&p.org_id)) {
        *shown.entry(p.leader.clone()).or_insert(0) += 1;
    }
    let shown_before = shown.clone();

    let mut moves = Vec::new();
    let mut blockers: Vec<PlanBlocker> = Vec::new();

    // Phase 1: back to the preferred node.
    for i in 0..units.len() {
        let unit = &units[i];
        if !unit.movable || unit.is_home_at_preferred() {
            continue;
        }
        let Some(preferred) = unit.preferred.clone() else {
            continue;
        };
        match check(input, unit, &preferred) {
            Ok(()) => {
                let planned = planned_move(&units[i], &preferred);
                apply_to_load(&mut load, &units[i], &preferred);
                if in_view(&units[i].org) {
                    apply_to_load(&mut shown, &units[i], &preferred);
                }
                units[i].moved = true;
                moves.push(planned);
            }
            Err((why, node)) => {
                if !is_lasting(why) && !input.automatic_return_off {
                    // Counted where it will be once the automatic return has
                    // dealt with it. A unit led from one node is returned
                    // when its preferred node has been in sync long enough:
                    // counted there. A split unit is not left waiting: the
                    // rule gathers its strays on the node that leads most of
                    // it at once, and the return follows.
                    let to = if units[i].led.len() > 1 {
                        units[i].home.clone()
                    } else {
                        preferred.clone()
                    };
                    apply_to_load(&mut load, &units[i], &to);
                    if in_view(&units[i].org) {
                        apply_to_load(&mut shown, &units[i], &to);
                    }
                    units[i].moved = true;
                }
                blockers.push(blocker(&units[i], why, node));
            }
        }
    }

    // Phase 2: equalize the nodes that answer. One that does not is left out
    // (it could not take a topic); one that answers but cannot take
    // forwarded transfers stays in, and `check` says why it receives nothing.
    let eligible: BTreeSet<String> = input
        .partitions
        .iter()
        .flat_map(|p| p.replicas.iter().chain(std::iter::once(&p.leader)))
        .filter(|n| input.reachable.contains(*n))
        .cloned()
        .collect();
    let smallest = units
        .iter()
        .filter(|u| u.movable)
        .map(|u| u.weight)
        .min()
        .unwrap_or(0);
    let mut equalizing_blockers: BTreeMap<(String, String), PlanBlocker> = BTreeMap::new();
    if eligible.len() >= 2 && smallest > 0 {
        while let Some((i, to)) = next_equalizing_move(
            input,
            &units,
            &load,
            &eligible,
            smallest,
            &mut equalizing_blockers,
        ) {
            let planned = planned_move(&units[i], &to);
            apply_to_load(&mut load, &units[i], &to);
            if in_view(&units[i].org) {
                apply_to_load(&mut shown, &units[i], &to);
            }
            units[i].moved = true;
            moves.push(planned);
        }
        let counts = eligible.iter().map(|n| load.get(n).copied().unwrap_or(0));
        let (low, high) = (counts.clone().min().unwrap_or(0), counts.max().unwrap_or(0));
        if high - low > smallest {
            let already: BTreeSet<(String, String)> = blockers
                .iter()
                .map(|b| (b.org_id.clone(), b.topic.clone()))
                .collect();
            let explained: BTreeSet<(String, String)> = already
                .iter()
                .cloned()
                .chain(equalizing_blockers.keys().cloned())
                .collect();
            blockers.extend(
                equalizing_blockers
                    .into_iter()
                    .filter(|(key, _)| !already.contains(key))
                    .map(|(_, b)| b),
            );
            // Nothing refused, yet the counts are still apart: no single unit
            // of the busiest node would improve them.
            let busiest = eligible
                .iter()
                .max_by(|a, b| {
                    let (ca, cb) = (
                        load.get(*a).copied().unwrap_or(0),
                        load.get(*b).copied().unwrap_or(0),
                    );
                    ca.cmp(&cb).then(b.cmp(a))
                })
                .cloned();
            if let Some(busiest) = busiest {
                blockers.extend(
                    units
                        .iter()
                        .filter(|u| u.movable && !u.moved && u.home == busiest)
                        .filter(|u| !explained.contains(&u.key()))
                        .map(|u| blocker(u, reason::WOULD_NOT_IMPROVE, Some(busiest.clone()))),
                );
            }
        }
    }
    blockers.sort_by(|a, b| (&a.org_id, &a.topic).cmp(&(&b.org_id, &b.topic)));

    let nodes: BTreeSet<String> = input
        .partitions
        .iter()
        .filter(|p| in_view(&p.org_id))
        .flat_map(|p| p.replicas.iter().chain(std::iter::once(&p.leader)).cloned())
        .chain(shown_before.keys().cloned())
        .chain(shown.keys().cloned())
        .collect();
    let per_node = nodes
        .into_iter()
        .map(|node| NodeLoad {
            before: shown_before.get(&node).copied().unwrap_or(0),
            after: shown.get(&node).copied().unwrap_or(0),
            node_id: node,
        })
        .collect();
    LeaderPlan {
        moves,
        per_node,
        blockers,
    }
}

fn build_units(input: &PlanInput) -> Vec<Unit> {
    let mut grouped: BTreeMap<(String, String), Vec<&PartitionFacts>> = BTreeMap::new();
    for p in &input.partitions {
        grouped
            .entry((p.org_id.clone(), unit_topic(&p.topic).to_string()))
            .or_default()
            .push(p);
    }
    grouped
        .into_iter()
        .map(|((org, topic), mut rows)| {
            rows.sort_by(|a, b| (&a.topic, a.partition).cmp(&(&b.topic, b.partition)));
            let mut led: BTreeMap<String, u32> = BTreeMap::new();
            for r in &rows {
                *led.entry(r.leader.clone()).or_insert(0) += 1;
            }
            // The preferred node is the one most rows name first, a tie going
            // to the node that leads most (`plurality_preferred`; the
            // automatic return reads it the same way).
            let preferred = plurality_preferred(
                rows.iter()
                    .map(|r| r.replicas.first().map(String::as_str).unwrap_or("")),
                &led,
            );
            let source_partitions: Vec<u32> = rows
                .iter()
                .filter(|r| r.topic == topic)
                .map(|r| r.partition)
                .collect();
            let in_scope = input.scope_org.as_ref().is_none_or(|o| *o == org)
                && input.scope_topic.as_ref().is_none_or(|t| *t == topic);
            // A dead-letter topic with no source topic stays where it is.
            let movable = in_scope && !is_internal(&topic) && !source_partitions.is_empty();
            Unit {
                org,
                topic,
                source_partitions,
                weight: rows.len() as u32,
                home: majority(&led),
                preferred,
                led,
                rows: rows
                    .iter()
                    .map(|r| RowFacts {
                        replicas: r.replicas.clone(),
                        isr: r.isr.clone(),
                        isr_live: r.isr_live,
                    })
                    .collect(),
                movable,
                moved: false,
            }
        })
        .collect()
}

fn planned_move(unit: &Unit, to: &str) -> PlannedMove {
    PlannedMove {
        org_id: unit.org.clone(),
        topic: unit.topic.clone(),
        partitions: unit.source_partitions.clone(),
        from: unit.home.clone(),
        to: to.to_string(),
        returns_to_preferred: unit.preferred.as_deref() == Some(to),
        isr_uncertain: unit.rows.iter().any(|r| !r.isr_live),
    }
}

fn blocker(unit: &Unit, reason: &str, node: Option<String>) -> PlanBlocker {
    PlanBlocker {
        org_id: unit.org.clone(),
        topic: unit.topic.clone(),
        reason: reason.to_string(),
        node_id: node,
    }
}

/// The load after `unit` goes to `to`: every row it leads moves.
fn apply_to_load(load: &mut BTreeMap<String, u32>, unit: &Unit, to: &str) {
    for (node, rows) in &unit.led {
        if let Some(count) = load.get_mut(node) {
            *count = count.saturating_sub(*rows);
        }
    }
    *load.entry(to.to_string()).or_insert(0) += unit.weight;
}

/// Why `unit` cannot go to `to`, in the order the reasons are reported.
fn check(input: &PlanInput, unit: &Unit, to: &str) -> Result<(), (&'static str, Option<String>)> {
    let leaders: Vec<&String> = unit.led.keys().collect();
    if let Some(down) = leaders
        .iter()
        .copied()
        .find(|n| !input.reachable.contains(*n))
    {
        return Err((reason::NODE_UNREACHABLE, Some(down.clone())));
    }
    if !input.reachable.contains(to) {
        return Err((reason::NODE_UNREACHABLE, Some(to.to_string())));
    }
    if let Some(old) = leaders
        .iter()
        .copied()
        .find(|n| !input.forward_capable.contains(*n))
    {
        return Err((reason::NODE_WITHOUT_FORWARDING, Some(old.clone())));
    }
    if !input.forward_capable.contains(to) {
        return Err((reason::NODE_WITHOUT_FORWARDING, Some(to.to_string())));
    }
    let key = unit.key();
    if input.paused.contains(&key)
        || input.paused.contains(&(
            unit.org.clone(),
            format!("{DLQ_TOPIC_PREFIX}{}", unit.topic),
        ))
    {
        return Err((reason::CONSUMER_GROUP_PAUSED, None));
    }
    // A ledger set (`isr_live == false`) is stale after an election and is
    // never widened again, so it does not refuse a move: the leader's own
    // check (`check_transfer`) judges the live set.
    if unit.rows.iter().any(|r| {
        !r.replicas.iter().any(|n| n == to) || (r.isr_live && !r.isr.iter().any(|n| n == to))
    }) {
        return Err((reason::TARGET_OUTSIDE_ISR, Some(to.to_string())));
    }
    if let Some((why, node)) = input.vetoed.get(&key) {
        return Err((
            veto_reason(why),
            node.clone().or_else(|| Some(to.to_string())),
        ));
    }
    Ok(())
}

/// A reason that will be the same next time: the return is not retried in the
/// hope that it passes, and the unit is counted where it is. The automatic
/// return (`leader_return`) holds the same two reasons that can arise on the
/// leader (a paused group, a preferred node without forwarding) to be lasting
/// and skips such a topic.
fn is_lasting(why: &str) -> bool {
    matches!(
        why,
        reason::NODE_WITHOUT_FORWARDING | reason::CONSUMER_GROUP_PAUSED | reason::NO_ADMIN_RIGHT
    )
}

/// A veto carries one of the `reason` constants; the check hands out
/// `&'static str`s, so the text is matched back to its constant.
fn veto_reason(why: &str) -> &'static str {
    [
        reason::NODE_UNREACHABLE,
        reason::NODE_WITHOUT_FORWARDING,
        reason::TARGET_OUTSIDE_ISR,
        reason::TARGET_BEHIND,
        reason::CONSUMER_GROUP_PAUSED,
        reason::NO_ADMIN_RIGHT,
    ]
    .into_iter()
    .find(|known| *known == why)
    .unwrap_or(reason::LEADER_UNAVAILABLE)
}

/// The next equalizing move, if any: `(unit index, target)`. Units that could
/// have moved but for a reason are noted in `blocked`.
fn next_equalizing_move(
    input: &PlanInput,
    units: &[Unit],
    load: &BTreeMap<String, u32>,
    eligible: &BTreeSet<String>,
    smallest: u32,
    blocked: &mut BTreeMap<(String, String), PlanBlocker>,
) -> Option<(usize, String)> {
    let count = |n: &String| load.get(n).copied().unwrap_or(0);
    let mut sources: Vec<&String> = eligible.iter().collect();
    sources.sort_by(|a, b| count(b).cmp(&count(a)).then(a.cmp(b)));
    let mut targets: Vec<&String> = eligible.iter().collect();
    targets.sort_by(|a, b| count(a).cmp(&count(b)).then(a.cmp(b)));
    for src in &sources {
        for dst in &targets {
            if src == dst || count(src) <= count(dst) {
                continue;
            }
            let gap = count(src) - count(dst);
            if gap <= smallest {
                continue;
            }
            let mut best: Option<(u32, u32, usize)> = None;
            for (i, unit) in units.iter().enumerate() {
                if !unit.movable || unit.moved || unit.home != **src || unit.weight >= gap {
                    continue;
                }
                if let Err((why, node)) = check(input, unit, dst) {
                    blocked
                        .entry(unit.key())
                        .or_insert_with(|| blocker(unit, why, node));
                    continue;
                }
                let left = gap.abs_diff(2 * unit.weight);
                let candidate = (left, unit.weight, i);
                if best.is_none_or(|b| candidate < b) {
                    best = Some(candidate);
                }
            }
            if let Some((_, _, i)) = best {
                return Some((i, (*dst).clone()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// `parts` partitions of `topic` (org-1), all led by `leader`, replicated
    /// on `replicas`, `replicas[0]` preferred, everyone in sync.
    fn topic(name: &str, parts: u32, leader: &str, replicas: &[&str]) -> Vec<PartitionFacts> {
        (0..parts)
            .map(|partition| PartitionFacts {
                org_id: "org-1".into(),
                topic: name.into(),
                partition,
                leader: leader.into(),
                replicas: replicas.iter().map(|s| s.to_string()).collect(),
                isr: replicas.iter().map(|s| s.to_string()).collect(),
                isr_live: true,
            })
            .collect()
    }

    fn input(partitions: Vec<PartitionFacts>) -> PlanInput {
        PlanInput {
            partitions,
            reachable: nodes(&["a", "b", "c"]),
            forward_capable: nodes(&["a", "b", "c"]),
            ..PlanInput::default()
        }
    }

    fn after(plan: &LeaderPlan, node: &str) -> u32 {
        plan.per_node
            .iter()
            .find(|n| n.node_id == node)
            .map_or(0, |n| n.after)
    }

    #[test]
    fn four_topics_on_one_node_spread_two_and_two_with_two_moves() {
        let mut parts = Vec::new();
        for name in ["t1", "t2", "t3", "t4"] {
            parts.extend(topic(name, 4, "a", &["a", "b"]));
        }
        let plan = plan(&input(parts));
        assert_eq!(plan.moves.len(), 2, "{plan:?}");
        assert_eq!((after(&plan, "a"), after(&plan, "b")), (8, 8));
        assert!(plan.moves.iter().all(|m| m.from == "a" && m.to == "b"));
        assert!(plan.moves.iter().all(|m| !m.returns_to_preferred));
        assert!(plan.blockers.is_empty());
        assert_eq!(
            plan.per_node,
            vec![
                NodeLoad {
                    node_id: "a".into(),
                    before: 16,
                    after: 8
                },
                NodeLoad {
                    node_id: "b".into(),
                    before: 0,
                    after: 8
                },
            ]
        );
    }

    #[test]
    fn a_balanced_instance_needs_no_move() {
        let mut parts = topic("t1", 4, "a", &["a", "b"]);
        parts.extend(topic("t2", 4, "b", &["b", "a"]));
        let plan = plan(&input(parts));
        assert!(plan.moves.is_empty());
        assert!(plan.blockers.is_empty());
    }

    #[test]
    fn a_gap_no_larger_than_the_smallest_topic_is_left_alone() {
        // 6 against 4, and the smallest topic has 2 partitions: no single
        // move can improve on a gap of 2.
        let mut parts = topic("t1", 4, "a", &["a", "b"]);
        parts.extend(topic("t2", 2, "a", &["a", "b"]));
        parts.extend(topic("t3", 4, "b", &["b", "a"]));
        let plan = plan(&input(parts));
        assert!(plan.moves.is_empty(), "{plan:?}");
    }

    #[test]
    fn a_topic_moves_with_its_dlq_as_one_unit() {
        let mut parts = topic("orders", 2, "a", &["a", "b"]);
        parts.extend(topic("__dlq.orders", 2, "a", &["a", "b"]));
        parts.extend(topic("other", 4, "a", &["a", "b"]));
        let plan = plan(&input(parts));
        assert_eq!(plan.moves.len(), 1, "{plan:?}");
        let moved = &plan.moves[0];
        assert_eq!(moved.topic, "orders");
        assert_eq!(moved.partitions, vec![0, 1], "the DLQ goes with them");
        // 4 rows of orders (2 + its DLQ's 2) and 4 of other: 4 and 4.
        assert_eq!((after(&plan, "a"), after(&plan, "b")), (4, 4));
    }

    #[test]
    fn a_topic_and_its_dlq_are_never_split_by_the_plan() {
        let mut parts = topic("orders", 2, "a", &["a", "b"]);
        parts.extend(topic("__dlq.orders", 2, "a", &["a", "b"]));
        let plan = plan(&input(parts));
        assert!(plan.moves.is_empty(), "one unit cannot be split: {plan:?}");
        assert_eq!((after(&plan, "a"), after(&plan, "b")), (4, 0));
    }

    #[test]
    fn the_dlq_partitions_weigh_into_the_unit() {
        let mut parts = topic("orders", 1, "a", &["a", "b"]);
        parts.extend(topic("__dlq.orders", 1, "a", &["a", "b"]));
        parts.extend(topic("big", 4, "a", &["a", "b"]));
        let plan = plan(&input(parts));
        // orders weighs 2: 6 on a, moving it leaves 4 and 2.
        assert_eq!(plan.moves.len(), 1, "{plan:?}");
        assert_eq!(plan.moves[0].topic, "orders");
        assert_eq!(plan.moves[0].partitions, vec![0]);
        assert_eq!((after(&plan, "a"), after(&plan, "b")), (4, 2));
    }

    #[test]
    fn topics_return_to_their_preferred_node_before_anything_is_equalized() {
        // `a` took everything over in a failover; `b` is preferred for t2
        // and t3, `a` for t1 and t4.
        let mut parts = Vec::new();
        parts.extend(topic("t1", 4, "a", &["a", "b"]));
        parts.extend(topic("t2", 4, "a", &["b", "a"]));
        parts.extend(topic("t3", 4, "a", &["b", "a"]));
        parts.extend(topic("t4", 4, "a", &["a", "b"]));
        let plan = plan(&input(parts));
        let names: Vec<&str> = plan.moves.iter().map(|m| m.topic.as_str()).collect();
        assert_eq!(names, vec!["t2", "t3"], "{plan:?}");
        assert!(plan.moves.iter().all(|m| m.returns_to_preferred));
        assert!(plan.moves.iter().all(|m| m.to == "b" && m.from == "a"));
        assert_eq!((after(&plan, "a"), after(&plan, "b")), (8, 8));
    }

    #[test]
    fn returns_come_first_in_the_output_and_equalizing_follows() {
        // t1 returns to b; afterwards a leads 12, b 4: one more topic moves.
        let mut parts = Vec::new();
        parts.extend(topic("t1", 4, "a", &["b", "a"]));
        parts.extend(topic("t2", 4, "a", &["a", "b"]));
        parts.extend(topic("t3", 4, "a", &["a", "b"]));
        parts.extend(topic("t4", 4, "a", &["a", "b"]));
        let plan = plan(&input(parts));
        assert_eq!(plan.moves[0].topic, "t1");
        assert!(plan.moves[0].returns_to_preferred);
        assert_eq!(plan.moves.len(), 2, "{plan:?}");
        assert!(!plan.moves[1].returns_to_preferred);
        assert_eq!((after(&plan, "a"), after(&plan, "b")), (8, 8));
    }

    #[test]
    fn a_unit_moves_at_most_once() {
        // t1 returns to b in phase 1; equalizing must not send it away again
        // (a leads 4, b 4 afterwards, nothing is left to do).
        let mut parts = Vec::new();
        parts.extend(topic("t1", 4, "a", &["b", "a"]));
        parts.extend(topic("t2", 4, "a", &["a", "b"]));
        let plan = plan(&input(parts));
        assert_eq!(plan.moves.iter().filter(|m| m.topic == "t1").count(), 1);
        assert_eq!(plan.moves.len(), 1);
    }

    #[test]
    fn three_nodes_even_out_by_the_smallest_topic() {
        let mut parts = Vec::new();
        for name in ["t1", "t2", "t3", "t4", "t5", "t6"] {
            parts.extend(topic(name, 2, "a", &["a", "b", "c"]));
        }
        let plan = plan(&input(parts));
        assert_eq!(plan.moves.len(), 4, "{plan:?}");
        let counts = ["a", "b", "c"].map(|n| after(&plan, n));
        assert_eq!(counts, [4, 4, 4]);
        assert!(plan.moves.iter().all(|m| m.from == "a"));
    }

    #[test]
    fn the_plan_is_the_same_whatever_order_the_facts_come_in() {
        let mut parts = Vec::new();
        for name in ["t1", "t2", "t3", "t4", "t5"] {
            parts.extend(topic(name, 2, "a", &["a", "b", "c"]));
        }
        let forward = plan(&input(parts.clone()));
        parts.reverse();
        let backward = plan(&input(parts.clone()));
        assert_eq!(forward, backward);
        assert_eq!(backward, plan(&input(parts)));
    }

    #[test]
    fn an_unreachable_leader_blocks_its_return_and_its_load_is_counted_on_the_preferred_node() {
        // t1 is led by c, which does not answer, though `a` is its preferred
        // node: it comes back to `a` when it can, so it weighs on `a` now and
        // only one of the topics `a` leads is moved to b.
        let mut parts = topic("t1", 4, "c", &["a", "c"]);
        parts.extend(topic("t2", 4, "a", &["a", "b"]));
        parts.extend(topic("t3", 4, "a", &["a", "b"]));
        let mut facts = input(parts);
        facts.reachable = nodes(&["a", "b"]);
        let plan = plan(&facts);
        assert_eq!(plan.moves.len(), 1, "{plan:?}");
        assert!(["t2", "t3"].contains(&plan.moves[0].topic.as_str()));
        assert_eq!((after(&plan, "a"), after(&plan, "b")), (8, 4));
        assert_eq!(
            plan.blockers,
            vec![PlanBlocker {
                org_id: "org-1".into(),
                topic: "t1".into(),
                reason: reason::NODE_UNREACHABLE.into(),
                node_id: Some("c".into()),
            }]
        );
    }

    #[test]
    fn an_unreachable_preferred_node_blocks_the_return() {
        let parts = topic("t1", 4, "a", &["b", "a"]);
        let mut facts = input(parts);
        facts.reachable = nodes(&["a"]);
        let plan = plan(&facts);
        assert!(plan.moves.is_empty());
        assert_eq!(plan.blockers.len(), 1);
        assert_eq!(plan.blockers[0].reason, reason::NODE_UNREACHABLE);
        assert_eq!(plan.blockers[0].node_id.as_deref(), Some("b"));
    }

    #[test]
    fn a_target_outside_the_isr_is_a_blocker_when_it_stops_the_balance() {
        let mut parts = topic("t1", 4, "a", &["a", "b"]);
        parts[1].isr = vec!["a".into()];
        let plan = plan(&input(parts));
        // One unit alone cannot be balanced anyway; with a second one that
        // is needed, the first one's ISR is the reason it stays.
        assert!(plan.moves.is_empty());

        let mut parts = topic("t1", 4, "a", &["a", "b"]);
        parts[1].isr = vec!["a".into()];
        parts.extend(topic("t2", 1, "a", &["a", "b"]));
        let plan = super::plan(&input(parts));
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].topic, "t2");
        assert_eq!(plan.blockers.len(), 1, "{plan:?}");
        assert_eq!(plan.blockers[0].topic, "t1");
        assert_eq!(plan.blockers[0].reason, reason::TARGET_OUTSIDE_ISR);
        assert_eq!(plan.blockers[0].node_id.as_deref(), Some("b"));
    }

    #[test]
    fn a_replica_set_without_the_target_is_outside_the_isr_too() {
        let mut parts = topic("t1", 4, "a", &["a", "c"]);
        parts.extend(topic("t2", 1, "a", &["a", "b"]));
        let plan = plan(&input(parts));
        assert_eq!(plan.blockers.len(), 1, "{plan:?}");
        assert_eq!(plan.blockers[0].reason, reason::TARGET_OUTSIDE_ISR);
    }

    #[test]
    fn a_blocker_is_not_reported_when_another_topic_reaches_the_balance() {
        let mut parts = topic("t1", 4, "a", &["a", "b"]);
        parts[1].isr = vec!["a".into()];
        parts.extend(topic("t2", 4, "a", &["a", "b"]));
        let plan = plan(&input(parts));
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].topic, "t2");
        assert!(plan.blockers.is_empty(), "{plan:?}");
    }

    #[test]
    fn a_ledger_isr_marks_the_move_uncertain() {
        let mut parts = topic("t1", 4, "a", &["a", "b"]);
        parts[0].isr_live = false;
        parts.extend(topic("t2", 4, "a", &["a", "b"]));
        let plan = plan(&input(parts));
        assert_eq!(plan.moves[0].topic, "t1");
        assert!(plan.moves[0].isr_uncertain);
        let mut parts = topic("t1", 4, "a", &["a", "b"]);
        parts.extend(topic("t2", 4, "a", &["a", "b"]));
        let plan = super::plan(&input(parts));
        assert!(!plan.moves[0].isr_uncertain);
    }

    #[test]
    fn a_paused_consumer_group_blocks_the_topic_and_its_dlq_name_counts_too() {
        for paused_name in ["t1", "__dlq.t1"] {
            let mut parts = topic("t1", 4, "a", &["a", "b"]);
            parts.extend(topic("__dlq.t1", 4, "a", &["a", "b"]));
            parts.extend(topic("t2", 1, "a", &["a", "b"]));
            let mut facts = input(parts);
            facts.paused.insert(("org-1".into(), paused_name.into()));
            let plan = plan(&facts);
            assert_eq!(plan.moves.len(), 1, "{paused_name}: {plan:?}");
            assert_eq!(plan.moves[0].topic, "t2");
            assert_eq!(plan.blockers.len(), 1, "{paused_name}: {plan:?}");
            assert_eq!(plan.blockers[0].reason, reason::CONSUMER_GROUP_PAUSED);
            assert_eq!(plan.blockers[0].topic, "t1");
        }
    }

    #[test]
    fn a_paused_topic_blocks_its_return_to_the_preferred_node() {
        let parts = topic("t1", 4, "a", &["b", "a"]);
        let mut facts = input(parts);
        facts.paused.insert(("org-1".into(), "t1".into()));
        let plan = plan(&facts);
        assert!(plan.moves.is_empty());
        assert_eq!(plan.blockers[0].reason, reason::CONSUMER_GROUP_PAUSED);
    }

    #[test]
    fn another_topic_takes_the_place_of_a_paused_one() {
        let mut parts = topic("t1", 4, "a", &["a", "b"]);
        parts.extend(topic("t2", 4, "a", &["a", "b"]));
        let mut facts = input(parts);
        facts.paused.insert(("org-1".into(), "t1".into()));
        let plan = plan(&facts);
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].topic, "t2");
        assert!(
            plan.blockers.is_empty(),
            "the balance was reached: {plan:?}"
        );
    }

    #[test]
    fn a_node_without_forwarding_receives_nothing_and_is_named() {
        let mut parts = topic("t1", 4, "a", &["a", "b"]);
        parts.extend(topic("t2", 4, "a", &["a", "b"]));
        let mut facts = input(parts);
        facts.forward_capable = nodes(&["a"]);
        let plan = plan(&facts);
        assert!(plan.moves.is_empty());
        assert_eq!(plan.blockers.len(), 2);
        assert!(plan
            .blockers
            .iter()
            .all(|b| b.reason == reason::NODE_WITHOUT_FORWARDING
                && b.node_id.as_deref() == Some("b")));
    }

    #[test]
    fn a_leader_without_forwarding_cannot_be_asked_to_give_a_topic_up() {
        let mut parts = topic("t1", 4, "b", &["b", "a"]);
        parts.extend(topic("t2", 4, "b", &["b", "a"]));
        let mut facts = input(parts);
        facts.forward_capable = nodes(&["a"]);
        let plan = plan(&facts);
        assert!(plan.moves.is_empty());
        assert_eq!(plan.blockers.len(), 2);
        assert!(plan
            .blockers
            .iter()
            .all(|b| b.reason == reason::NODE_WITHOUT_FORWARDING
                && b.node_id.as_deref() == Some("b")));
    }

    #[test]
    fn a_leader_without_forwarding_blocks_the_return_of_its_topic() {
        let parts = topic("t1", 4, "b", &["a", "b"]);
        let mut facts = input(parts);
        facts.forward_capable = nodes(&["a"]);
        let plan = plan(&facts);
        assert_eq!(plan.blockers.len(), 1);
        assert_eq!(plan.blockers[0].reason, reason::NODE_WITHOUT_FORWARDING);
        assert_eq!(plan.blockers[0].node_id.as_deref(), Some("b"));
    }

    #[test]
    fn a_veto_from_a_leader_becomes_a_blocker_and_the_plan_goes_on_without_it() {
        let mut parts = topic("t1", 4, "a", &["a", "b"]);
        parts.extend(topic("t2", 4, "a", &["a", "b"]));
        let mut facts = input(parts);
        facts.vetoed.insert(
            ("org-1".into(), "t1".into()),
            (reason::TARGET_BEHIND.to_string(), Some("b".into())),
        );
        let plan = plan(&facts);
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].topic, "t2");

        // Only t1 could move, and the leader refused it.
        let mut parts = topic("t1", 4, "a", &["a", "b"]);
        parts.extend(topic("tiny", 1, "a", &["a", "b"]));
        let mut facts = input(parts);
        facts.vetoed.insert(
            ("org-1".into(), "t1".into()),
            (reason::TARGET_BEHIND.to_string(), Some("b".into())),
        );
        let plan = super::plan(&facts);
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].topic, "tiny");
        assert_eq!(plan.blockers.len(), 1);
        assert_eq!(plan.blockers[0].reason, reason::TARGET_BEHIND);
    }

    #[test]
    fn a_veto_with_an_unknown_reason_reads_as_an_unavailable_leader() {
        let mut parts = topic("t1", 4, "a", &["a", "b"]);
        parts.extend(topic("tiny", 1, "a", &["a", "b"]));
        let mut facts = input(parts);
        facts.vetoed.insert(
            ("org-1".into(), "t1".into()),
            ("something_new".to_string(), None),
        );
        let plan = plan(&facts);
        assert_eq!(plan.blockers[0].reason, reason::LEADER_UNAVAILABLE);
        assert_eq!(plan.blockers[0].node_id.as_deref(), Some("b"));
    }

    #[test]
    fn other_organisations_count_as_load_but_never_move() {
        let mut parts = Vec::new();
        for name in ["x1", "x2"] {
            parts.extend(topic(name, 4, "a", &["a", "b"]).into_iter().map(|mut p| {
                p.org_id = "org-2".into();
                p
            }));
        }
        parts.extend(topic("mine1", 4, "a", &["a", "b"]));
        parts.extend(topic("mine2", 4, "a", &["a", "b"]));
        let mut facts = input(parts);
        facts.scope_org = Some("org-1".into());
        let plan = plan(&facts);
        assert!(plan.moves.iter().all(|m| m.org_id == "org-1"));
        // 16 on a; only org-1's two topics can move, one after the other.
        // `per_node` is org-1's own view: its 8 partitions all end on b, which
        // is what evens the whole instance out (8 and 8 with org-2's 8 on a).
        assert_eq!(plan.moves.len(), 2);
        assert_eq!((after(&plan, "a"), after(&plan, "b")), (0, 8));
        assert_eq!(plan.per_node.iter().map(|n| n.before).sum::<u32>(), 8);
    }

    #[test]
    fn scoping_to_one_topic_moves_nothing_else() {
        let mut parts = Vec::new();
        for name in ["t1", "t2", "t3"] {
            parts.extend(topic(name, 4, "a", &["a", "b"]));
        }
        let mut facts = input(parts);
        facts.scope_topic = Some("t2".into());
        let plan = plan(&facts);
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].topic, "t2");
    }

    #[test]
    fn internal_topics_stay_put_but_count() {
        let mut parts = topic("__bus.metrics", 1, "a", &["a"]);
        parts.extend(topic("t1", 1, "a", &["a", "b"]));
        parts.extend(topic("t2", 1, "a", &["a", "b"]));
        let plan = plan(&input(parts));
        assert!(plan.moves.iter().all(|m| m.topic != "__bus.metrics"));
        assert_eq!(plan.per_node[0].node_id, "a");
        assert_eq!(plan.per_node[0].before, 3);
    }

    #[test]
    fn a_unit_with_stray_partitions_is_gathered_on_its_preferred_node() {
        let mut parts = topic("t1", 3, "a", &["a", "b"]);
        parts[2].leader = "b".into();
        let plan = plan(&input(parts));
        assert_eq!(plan.moves.len(), 1, "{plan:?}");
        assert_eq!(plan.moves[0].to, "a");
        assert!(plan.moves[0].returns_to_preferred);
    }

    #[test]
    fn one_node_alone_has_nothing_to_balance() {
        let mut parts = topic("t1", 4, "a", &["a"]);
        parts.extend(topic("t2", 4, "a", &["a"]));
        let mut facts = input(parts);
        facts.reachable = nodes(&["a"]);
        let plan = plan(&facts);
        assert!(plan.moves.is_empty());
        assert!(plan.blockers.is_empty());
    }

    #[test]
    fn the_equalizing_move_is_the_one_that_leaves_the_smallest_gap() {
        // 12 on a, none on b; units of 8, 3 and 1. Moving the 8 leaves 4/8
        // (gap 4), the 3 leaves 9/3 (gap 6), the 1 leaves 11/1.
        let mut parts = topic("big", 8, "a", &["a", "b"]);
        parts.extend(topic("mid", 3, "a", &["a", "b"]));
        parts.extend(topic("tiny", 1, "a", &["a", "b"]));
        let plan = plan(&input(parts));
        assert_eq!(plan.moves.len(), 1, "{plan:?}");
        assert_eq!(plan.moves[0].topic, "big");
        assert_eq!((after(&plan, "a"), after(&plan, "b")), (4, 8));
        // 8 against 4 is the best that units of 8, 3 and 1 allow, so nothing
        // is left unexplained.
        assert!(plan.blockers.is_empty(), "{plan:?}");
    }

    #[test]
    fn a_topic_that_would_not_improve_the_counts_is_named_with_its_reason() {
        // a leads 11 (a unit of 10 and one of 1), b leads 5. Moving the 1 is
        // the only improvement; the unit of 10 would only swap the sides.
        let mut parts = topic("big", 10, "a", &["a", "b"]);
        parts.extend(topic("x", 1, "a", &["a", "b"]));
        parts.extend(topic("y", 5, "b", &["b", "a"]));
        let plan = plan(&input(parts));
        assert_eq!(plan.moves.len(), 1, "{plan:?}");
        assert_eq!(plan.moves[0].topic, "x");
        assert_eq!(
            plan.blockers,
            vec![PlanBlocker {
                org_id: "org-1".into(),
                topic: "big".into(),
                reason: reason::WOULD_NOT_IMPROVE.into(),
                node_id: Some("a".into()),
            }]
        );
    }

    /// TB-04 §8: b was down, a took over everything, and the ledger's ISR (a
    /// set that is never widened after an election) still reads [a]. When b
    /// is back and the plan is made on b (whose own view of the ISR is the
    /// stale one), the returns are proposed: the leader's own check judges
    /// the live set.
    #[test]
    fn a_stale_ledger_isr_does_not_veto_the_return_of_the_topics_to_the_node_that_came_back() {
        let mut parts = Vec::new();
        for name in ["t1", "t2"] {
            let mut p = topic(name, 4, "a", &["b", "a"]);
            for part in &mut p {
                part.isr = vec!["a".into()];
                part.isr_live = false;
            }
            parts.extend(p);
        }
        let plan = plan(&input(parts));
        assert!(plan.blockers.is_empty(), "{plan:?}");
        assert_eq!(plan.moves.len(), 2, "{plan:?}");
        assert!(plan
            .moves
            .iter()
            .all(|m| m.to == "b" && m.returns_to_preferred && m.isr_uncertain));
        // The same ISR read live from the leader is a real veto.
        let mut parts = topic("t1", 4, "a", &["b", "a"]);
        for part in &mut parts {
            part.isr = vec!["a".into()];
        }
        let plan = super::plan(&input(parts));
        assert!(plan.moves.is_empty(), "{plan:?}");
        assert_eq!(plan.blockers[0].reason, reason::TARGET_OUTSIDE_ISR);
    }

    /// A return that is blocked only for now does not turn into a move of
    /// something else to the node it waits for: the unit counts on its
    /// preferred node, and with that the three nodes are even.
    #[test]
    fn a_return_blocked_for_now_counts_its_load_on_the_preferred_node() {
        // t1 prefers a but b leads it (a is behind); t2 and t3 are at home.
        let mut parts = topic("t1", 4, "b", &["a", "b"]);
        parts.extend(topic("t2", 4, "b", &["b", "c"]));
        parts.extend(topic("t3", 4, "c", &["c", "a"]));
        let mut facts = input(parts);
        facts.vetoed.insert(
            ("org-1".into(), "t1".into()),
            (reason::TARGET_BEHIND.to_string(), Some("a".into())),
        );
        let plan = plan(&facts);
        assert!(plan.moves.is_empty(), "{plan:?}");
        assert_eq!(plan.blockers.len(), 1, "{plan:?}");
        assert_eq!(plan.blockers[0].reason, reason::TARGET_BEHIND);
        assert_eq!(
            (after(&plan, "a"), after(&plan, "b"), after(&plan, "c")),
            (4, 4, 4)
        );
    }

    /// A split topic whose return is blocked for now is counted on the node
    /// that leads most of it: the automatic rule gathers it there at once
    /// instead of waiting for the preferred node.
    #[test]
    fn a_split_topic_blocked_for_now_counts_on_the_node_that_leads_most_of_it() {
        let mut parts = topic("t1", 2, "b", &["a", "b", "c"]);
        let mut stray = topic("t1", 1, "c", &["a", "b", "c"]);
        stray[0].partition = 2;
        parts.extend(stray);
        let mut facts = input(parts);
        facts.vetoed.insert(
            ("org-1".into(), "t1".into()),
            (reason::TARGET_BEHIND.to_string(), Some("a".into())),
        );
        let plan = plan(&facts);
        assert!(plan.moves.is_empty(), "{plan:?}");
        assert_eq!(
            (after(&plan, "a"), after(&plan, "b"), after(&plan, "c")),
            (0, 3, 0),
            "{plan:?}"
        );
    }

    /// Rows that disagree on who is preferred take the node most of them
    /// name, as they do for the automatic return; a tie goes to the node
    /// that leads the unit, so a tied unit stays where it is.
    #[test]
    fn a_unit_whose_rows_disagree_on_the_preferred_node_takes_the_majority() {
        let mut parts = topic("t1", 1, "b", &["a", "b"]);
        let mut other = topic("t1", 1, "b", &["b", "a"]);
        other[0].partition = 1;
        parts.extend(other);
        let tied = plan(&input(parts));
        assert!(tied.moves.is_empty(), "a tie goes to the leader: {tied:?}");
        assert!(tied.blockers.is_empty(), "{tied:?}");

        // Three of four rows prefer `a`, so a unit led by `b` returns to it.
        let mut parts = topic("t1", 4, "b", &["a", "b"]);
        parts[1].replicas = vec!["b".into(), "a".into()];
        let majority_plan = plan(&input(parts));
        assert_eq!(majority_plan.moves.len(), 1, "{majority_plan:?}");
        assert_eq!(majority_plan.moves[0].to, "a");
        assert!(majority_plan.moves[0].returns_to_preferred);
    }

    #[test]
    fn plurality_preferred_breaks_a_tie_to_the_leader_and_then_the_lowest_id() {
        let led = |pairs: &[(&str, u32)]| -> BTreeMap<String, u32> {
            pairs.iter().map(|(n, c)| (n.to_string(), *c)).collect()
        };
        assert_eq!(
            plurality_preferred(["a", "a", "b"], &led(&[("b", 3)])).as_deref(),
            Some("a")
        );
        assert_eq!(
            plurality_preferred(["a", "b"], &led(&[("b", 2)])).as_deref(),
            Some("b")
        );
        assert_eq!(
            plurality_preferred(["b", "a"], &led(&[("c", 2)])).as_deref(),
            Some("a")
        );
        assert_eq!(plurality_preferred(["", ""], &led(&[])), None);
    }

    /// With the automatic return switched off nothing brings the topic back
    /// by itself, so a return blocked for now counts where the unit is.
    #[test]
    fn a_return_blocked_for_now_counts_where_the_unit_is_when_the_automatic_return_is_off() {
        let mut parts = topic("t1", 4, "b", &["a", "b"]);
        parts.extend(topic("t2", 4, "b", &["b", "c"]));
        parts.extend(topic("t3", 4, "c", &["c", "a"]));
        let mut facts = input(parts);
        facts.automatic_return_off = true;
        facts.vetoed.insert(
            ("org-1".into(), "t1".into()),
            (reason::TARGET_BEHIND.to_string(), Some("a".into())),
        );
        let plan = plan(&facts);
        assert!(plan.moves.iter().all(|m| m.topic != "t1"), "{plan:?}");
        assert_eq!(
            (after(&plan, "a"), after(&plan, "b"), after(&plan, "c")),
            (0, 8, 4),
            "t1 stays counted on b: {plan:?}"
        );
    }

    /// A return that will never pass (a paused group) leaves the unit where
    /// it is, counted where it is, and the rest is balanced around it.
    #[test]
    fn a_return_blocked_for_good_counts_where_the_unit_is() {
        let mut parts = topic("t1", 4, "b", &["a", "b"]);
        parts.extend(topic("t2", 4, "b", &["b", "a"]));
        let mut facts = input(parts);
        facts.paused.insert(("org-1".into(), "t1".into()));
        let plan = plan(&facts);
        assert_eq!(plan.moves.len(), 1, "{plan:?}");
        assert_eq!(plan.moves[0].topic, "t2");
        assert_eq!((after(&plan, "a"), after(&plan, "b")), (4, 4));
    }

    #[test]
    fn per_node_counts_only_the_partitions_of_the_organisation_asked_about() {
        let mut parts = topic("mine", 4, "a", &["a", "b"]);
        parts.extend(topic("mine2", 4, "a", &["a", "b"]));
        parts.extend(
            topic("theirs", 6, "b", &["b", "a"])
                .into_iter()
                .map(|mut p| {
                    p.org_id = "org-2".into();
                    p
                }),
        );
        let mut facts = input(parts);
        facts.scope_org = Some("org-1".into());
        let plan = plan(&facts);
        let total_before: u32 = plan.per_node.iter().map(|n| n.before).sum();
        let total_after: u32 = plan.per_node.iter().map(|n| n.after).sum();
        assert_eq!((total_before, total_after), (8, 8), "{plan:?}");
    }
}
