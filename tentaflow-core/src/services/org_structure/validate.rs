//! Pure rules of the org structure: date parsing, half-open intervals and the
//! checks SQLite cannot express as constraints. Nothing here touches the
//! database, so every rule is testable on plain values.

use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::NaiveDate;

use super::error::{OrgStructureError, Result};

const DATE_FORMAT: &str = "%Y-%m-%d";

/// Longest name and code the structure stores. Rows replicate to every node,
/// so the wire and the file import bound them before anything is written.
pub const MAX_NAME_CHARS: usize = 200;
pub const MAX_CODE_CHARS: usize = 50;

/// Shares above this are a warning, not an error (overtime, transitions).
pub const FULL_TIME: f64 = 1.0;
const SHARE_EPSILON: f64 = 1e-9;

pub fn parse_date(raw: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(raw, DATE_FORMAT)
        .map_err(|_| OrgStructureError::InvalidDate(raw.to_string()))
}

pub fn format_date(date: NaiveDate) -> String {
    date.format(DATE_FORMAT).to_string()
}

/// The organization's calendar day right now. The day is the organization's,
/// not the server's: a change at 23:30 UTC is already tomorrow in Warsaw.
pub fn today_in_zone(timezone: &str) -> Result<NaiveDate> {
    let zone: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| OrgStructureError::InvalidTimezone(timezone.to_string()))?;
    Ok(chrono::Utc::now().with_timezone(&zone).date_naive())
}

/// `[from, to)`; `to == None` is open-ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub from: NaiveDate,
    pub to: Option<NaiveDate>,
}

impl Interval {
    pub fn new(from: NaiveDate, to: Option<NaiveDate>) -> Result<Self> {
        if let Some(end) = to {
            if end <= from {
                return Err(OrgStructureError::InvalidInterval {
                    from: format_date(from),
                    to: format_date(end),
                });
            }
        }
        Ok(Self { from, to })
    }

    pub fn contains(&self, day: NaiveDate) -> bool {
        self.from <= day && self.to.is_none_or(|end| day < end)
    }

    pub fn overlaps(&self, other: &Interval) -> bool {
        let starts_before_other_ends = other.to.is_none_or(|end| self.from < end);
        let other_starts_before_end = self.to.is_none_or(|end| other.from < end);
        starts_before_other_ends && other_starts_before_end
    }

    /// True when `self` lies wholly inside `outer`.
    pub fn is_within(&self, outer: &Interval) -> bool {
        outer.from <= self.from
            && match (outer.to, self.to) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(outer_end), Some(end)) => end <= outer_end,
            }
    }
}

/// True when the union of `pieces` covers `target` without a gap. Versions of
/// one unit are contiguous by construction, but a unit that was liquidated and
/// (wrongly) referenced past that date must not pass.
pub fn is_covered(pieces: &[Interval], target: &Interval) -> bool {
    let mut sorted: Vec<&Interval> = pieces.iter().collect();
    sorted.sort_by_key(|i| i.from);
    let mut cursor = target.from;
    for piece in sorted {
        if piece.from > cursor {
            break;
        }
        if !piece.contains(cursor) {
            continue;
        }
        match piece.to {
            None => return true,
            Some(end) => {
                cursor = end;
                if target.to.is_some_and(|t| cursor >= t) {
                    return true;
                }
            }
        }
    }
    false
}

/// First day on which two of `intervals` are both valid, if any.
pub fn first_overlap(intervals: &[Interval]) -> Option<NaiveDate> {
    for (i, a) in intervals.iter().enumerate() {
        for b in &intervals[i + 1..] {
            if a.overlaps(b) {
                return Some(a.from.max(b.from));
            }
        }
    }
    None
}

/// One day per stretch inside `within` over which nothing in `boundaries`
/// changes. Checking these days is checking every day of the interval.
pub fn segment_starts(
    within: &Interval,
    boundaries: impl IntoIterator<Item = NaiveDate>,
) -> Vec<NaiveDate> {
    let mut starts = BTreeSet::new();
    starts.insert(within.from);
    for day in boundaries {
        if day > within.from && within.to.is_none_or(|end| day < end) {
            starts.insert(day);
        }
    }
    starts.into_iter().collect()
}

/// A parent link that holds over an interval: a reporting line or the
/// parent of a unit version.
#[derive(Debug, Clone)]
pub struct Edge {
    pub child: String,
    pub parent: String,
    pub interval: Interval,
}

/// First day inside `within` on which following parents from `child` leads
/// back to `child`. `edges` must already contain the link being validated.
///
/// The check covers every stretch of `within`, not only today: a link that is
/// harmless now can close a loop with a reorganization dated next month.
pub fn find_cycle(edges: &[Edge], child: &str, within: &Interval) -> Option<NaiveDate> {
    // A loop through `child` only contains ancestors of `child` (on some day),
    // so everything outside that closure cannot matter.
    let mut relevant: HashSet<&str> = HashSet::from([child]);
    loop {
        let before = relevant.len();
        for edge in edges {
            if relevant.contains(edge.child.as_str()) {
                relevant.insert(edge.parent.as_str());
            }
        }
        if relevant.len() == before {
            break;
        }
    }
    let edges: Vec<&Edge> = edges
        .iter()
        .filter(|e| relevant.contains(e.child.as_str()))
        .collect();
    let boundaries = edges
        .iter()
        .flat_map(|e| std::iter::once(e.interval.from).chain(e.interval.to));
    segment_starts(within, boundaries)
        .into_iter()
        .find(|&day| cycle_through(edges.iter().copied(), child, day).is_some())
}

/// A loop through `start` on `day`, as its members starting with `start`.
/// Follows EVERY parent of a node: a structure merged from two offline edits
/// can give a node two parents on one day, and the loop may run through either.
pub fn cycle_through<'a>(
    edges: impl IntoIterator<Item = &'a Edge>,
    start: &str,
    day: NaiveDate,
) -> Option<Vec<String>> {
    let mut parents: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges.into_iter().filter(|e| e.interval.contains(day)) {
        parents
            .entry(edge.child.as_str())
            .or_default()
            .push(edge.parent.as_str());
    }
    let mut visited: HashSet<&str> = HashSet::new();
    let mut path: Vec<&str> = vec![start];
    let mut cursor: Vec<usize> = vec![0];
    while let Some(&node) = path.last() {
        let next = parents
            .get(node)
            .and_then(|p| p.get(*cursor.last().expect("cursor per path node")));
        match next {
            None => {
                visited.insert(node);
                path.pop();
                cursor.pop();
            }
            Some(&parent) => {
                *cursor.last_mut().expect("cursor per path node") += 1;
                if parent == start {
                    return Some(path.iter().map(|n| n.to_string()).collect());
                }
                // Loops that do not include `start` are not this link's doing.
                if !visited.contains(parent) && !path.contains(&parent) {
                    path.push(parent);
                    cursor.push(0);
                }
            }
        }
    }
    None
}

/// Highest total share of one person over `within`, with the first day it is
/// reached.
pub fn peak_share(rows: &[(Interval, f64)], within: &Interval) -> Option<(NaiveDate, f64)> {
    let boundaries = rows
        .iter()
        .flat_map(|(i, _)| std::iter::once(i.from).chain(i.to));
    segment_starts(within, boundaries)
        .into_iter()
        .map(|day| {
            let total: f64 = rows
                .iter()
                .filter(|(i, _)| i.contains(day))
                .map(|(_, share)| share)
                .sum();
            (day, total)
        })
        .fold(None, |best: Option<(NaiveDate, f64)>, cur| match best {
            Some(b) if b.1 >= cur.1 => Some(b),
            _ => Some(cur),
        })
}

pub fn share_exceeds_full_time(total: f64) -> bool {
    total > FULL_TIME + SHARE_EPSILON
}

pub fn require_non_empty(field: &'static str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(OrgStructureError::EmptyField { field });
    }
    Ok(trimmed.to_string())
}

/// `Some("")` means "none": the UI sends empty strings for cleared inputs.
pub fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

pub fn validate_share(share: f64) -> Result<()> {
    if !share.is_finite() || share <= 0.0 || share > FULL_TIME {
        return Err(OrgStructureError::InvalidValue {
            field: "share",
            reason: "must be greater than 0 and at most 1".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 3, day).unwrap()
    }

    fn iv(from: u32, to: Option<u32>) -> Interval {
        Interval::new(d(from), to.map(d)).unwrap()
    }

    fn edge(child: &str, parent: &str, from: u32, to: Option<u32>) -> Edge {
        Edge {
            child: child.into(),
            parent: parent.into(),
            interval: iv(from, to),
        }
    }

    #[test]
    fn interval_is_half_open() {
        let a = iv(1, Some(10));
        assert!(a.contains(d(1)));
        assert!(!a.contains(d(10)));
        // Touching intervals do not overlap: the old row ends the day the new one starts.
        assert!(!a.overlaps(&iv(10, None)));
        assert!(a.overlaps(&iv(9, None)));
    }

    #[test]
    fn interval_rejects_empty_and_reversed() {
        assert!(Interval::new(d(5), Some(d(5))).is_err());
        assert!(Interval::new(d(5), Some(d(4))).is_err());
    }

    #[test]
    fn coverage_needs_contiguous_pieces() {
        let pieces = [iv(1, Some(10)), iv(10, None)];
        assert!(is_covered(&pieces, &iv(5, None)));
        let gapped = [iv(1, Some(10)), iv(12, None)];
        assert!(!is_covered(&gapped, &iv(5, None)));
        assert!(is_covered(&gapped, &iv(2, Some(9))));
    }

    #[test]
    fn cycle_in_a_future_interval_only_is_found() {
        // a -> b always; b -> a only from day 20.
        let edges = [edge("a", "b", 1, None), edge("b", "a", 20, None)];
        assert_eq!(find_cycle(&edges, "b", &iv(20, None)), Some(d(20)));
        // The same link checked over a window that ends before day 20 is fine.
        assert_eq!(find_cycle(&edges, "b", &iv(1, Some(20))), None);
    }

    #[test]
    fn cycle_check_ignores_loops_not_through_the_child() {
        let edges = [
            edge("x", "y", 1, None),
            edge("y", "x", 1, None),
            edge("c", "x", 1, None),
        ];
        assert_eq!(find_cycle(&edges, "c", &iv(1, None)), None);
    }

    #[test]
    fn peak_share_sums_overlapping_rows_only() {
        let rows = [(iv(1, Some(10)), 0.6), (iv(5, None), 0.6)];
        let (day, total) = peak_share(&rows, &iv(5, None)).unwrap();
        assert_eq!(day, d(5));
        assert!(share_exceeds_full_time(total));
        let sequential = [(iv(1, Some(10)), 0.6), (iv(10, None), 0.6)];
        let (_, total) = peak_share(&sequential, &iv(1, None)).unwrap();
        assert!(!share_exceeds_full_time(total));
    }

    #[test]
    fn dates_round_trip_and_reject_garbage() {
        assert_eq!(format_date(parse_date("2026-03-01").unwrap()), "2026-03-01");
        assert!(parse_date("01.03.2026").is_err());
        assert!(today_in_zone("Europe/Warsaw").is_ok());
        assert!(today_in_zone("Mars/Base").is_err());
    }
}
