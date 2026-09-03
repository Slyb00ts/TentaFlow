// =============================================================================
// File: tentanas/arc.rs — the ZFS ARC of one node (plan-02 §5.2, the ARC card
//       and its slider). Reading is entirely unprivileged: the kernel exposes
//       the counters in `/proc/spl/kstat/zfs/arcstats` and the current cap in
//       `/sys/module/zfs/parameters/zfs_arc_max`, both world-readable. Only
//       CHANGING the cap needs the privilege channel, and that goes through
//       the helper's `ArcLimitSet` builtin so the runtime value and the
//       modprobe drop-in are written together.
//
//       `limit_source` is what makes the card honest: a cap the admin set
//       through the app survives a reboot (the drop-in), one someone echoed
//       into sysfs by hand does not, and ZFS's own default is neither.
// =============================================================================

use std::collections::HashMap;
use std::path::Path;

use tentaflow_protocol::tentanas::NasArcStats;

/// Where the kernel publishes the ARC counters. A node without the zfs module
/// does not have it, which is how `stats` knows to answer `None`.
const ARCSTATS_PATH: &str = "/proc/spl/kstat/zfs/arcstats";

/// `name  type  data` rows, three whitespace-separated columns after a
/// two-line header. Unknown names are kept: the set differs between OpenZFS
/// releases and this parser must not care.
pub fn parse_arcstats(text: &str) -> HashMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let name = cols.next()?;
            let _kind = cols.next()?;
            let value = cols.next()?.parse::<u64>().ok()?;
            Some((name.to_string(), value))
        })
        .collect()
}

fn sum(stats: &HashMap<String, u64>, names: &[&str]) -> u64 {
    names.iter().filter_map(|n| stats.get(*n)).sum()
}

/// Percentage of ARC lookups served from cache since boot. Zero lookups is
/// 0 %, not a division by zero — a freshly booted node reports it that way.
pub fn hit_ratio(hits: u64, misses: u64) -> f64 {
    let total = hits + misses;
    if total == 0 {
        return 0.0;
    }
    // Two decimals: the card shows one, and a full f64 ratio would make the
    // wire value differ between two reads of the same counters.
    ((hits as f64 / total as f64) * 10_000.0).round() / 100.0
}

/// The cap the running module enforces, or `None` when the parameter is not
/// readable (no zfs module).
fn sysfs_max() -> Option<u64> {
    std::fs::read_to_string(tentanas_helper::ARC_MAX_SYSFS_PATH)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
}

/// The value the app's own modprobe drop-in persists, when it is there.
/// Parsed rather than assumed so a hand-edited file reports what it says.
pub fn parse_modprobe_max(text: &str) -> Option<u64> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once("zfs_arc_max="))
        .filter_map(|(_, rest)| rest.split_whitespace().next())
        .find_map(|v| v.parse::<u64>().ok())
}

fn modprobe_max() -> Option<u64> {
    std::fs::read_to_string(tentanas_helper::ARC_MODPROBE_PATH)
        .ok()
        .as_deref()
        .and_then(parse_modprobe_max)
}

/// Where the cap comes from (§5.2): 'modprobe' when the app's drop-in sets it
/// and the running module agrees, 'runtime' when only the module has a
/// non-default cap, 'default' when nothing set one.
pub fn limit_source(sysfs: Option<u64>, drop_in: Option<u64>) -> &'static str {
    match (sysfs, drop_in) {
        // The drop-in only takes effect at module load, so a mismatch means
        // someone changed the running value afterwards: the truthful label is
        // the one describing what is in force NOW.
        (Some(live), Some(persisted)) if live == persisted => "modprobe",
        (Some(live), _) if live > 0 => "runtime",
        (_, Some(_)) => "modprobe",
        _ => "default",
    }
}

/// The ARC of this node, or `None` when it has no ZFS at all. `slog_pools`
/// and `l2arc_pools` come from the pool list the caller already has, so this
/// costs no extra `zpool` call.
pub fn stats(pools: &[tentaflow_protocol::tentanas::NasPool]) -> Option<NasArcStats> {
    let text = std::fs::read_to_string(ARCSTATS_PATH).ok()?;
    let s = parse_arcstats(&text);
    if s.is_empty() {
        return None;
    }
    let live = sysfs_max();
    let drop_in = modprobe_max();
    // A zero module parameter means "no explicit cap", and then the number to
    // show is `c_max`: the size ZFS picked for itself, which is what the
    // slider must start from.
    let max_bytes = live.filter(|v| *v > 0).or_else(|| s.get("c_max").copied()).unwrap_or(0);
    let hits = sum(&s, &["hits"]);
    let misses = sum(&s, &["misses"]);
    let pools_with = |role: &str| -> Vec<String> {
        pools
            .iter()
            .filter(|p| p.vdevs.iter().any(|v| v.role == role))
            .map(|p| p.name.clone())
            .collect()
    };
    Some(NasArcStats {
        size_bytes: sum(&s, &["size"]),
        max_bytes,
        min_bytes: sum(&s, &["c_min"]),
        // The same reader the helper's own bound check uses, so core's
        // ceiling and the catalog's cannot be computed from different RAM.
        ram_bytes: tentanas_helper::meminfo_total_bytes(),
        hit_ratio: hit_ratio(hits, misses),
        mru_bytes: sum(&s, &["mru_size"]),
        mfu_bytes: sum(&s, &["mfu_size"]),
        demand_hits: sum(&s, &["demand_data_hits", "demand_metadata_hits"]),
        prefetch_hits: sum(&s, &["prefetch_data_hits", "prefetch_metadata_hits"]),
        slog_pools: pools_with("log"),
        l2arc_pools: pools_with("cache"),
        limit_source: limit_source(live, drop_in).to_string(),
    })
}

/// Whether this node has an ARC to talk about at all — the guard the ARC
/// limit handler uses before it asks for a privilege channel.
pub fn present() -> bool {
    Path::new(ARCSTATS_PATH).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed but real `arcstats` head, including the two header lines the
    /// parser has to walk past on its own.
    const FIXTURE: &str = "13 1 0x01 98 26656 4162849911 20035443063241\n\
name                            type data\n\
hits                            4    1846201\n\
misses                          4    48312\n\
demand_data_hits                4    901233\n\
demand_metadata_hits            4    712004\n\
prefetch_data_hits              4    180011\n\
prefetch_metadata_hits          4    52953\n\
mru_size                        4    3221225472\n\
mfu_size                        4    4294967296\n\
size                            4    8053063680\n\
c_min                           4    1073741824\n\
c_max                           4    17179869184\n";

    #[test]
    fn the_arcstats_fixture_parses_into_the_card_numbers() {
        let s = parse_arcstats(FIXTURE);
        // The header lines have three columns too, so the parser must reject
        // them by their non-numeric third field, not by counting lines.
        assert!(!s.contains_key("name"));
        assert_eq!(s.get("size"), Some(&8_053_063_680));
        assert_eq!(s.get("c_max"), Some(&17_179_869_184));
        assert_eq!(sum(&s, &["demand_data_hits", "demand_metadata_hits"]), 1_613_237);
        assert_eq!(sum(&s, &["prefetch_data_hits", "prefetch_metadata_hits"]), 232_964);
        // An unknown counter is simply absent, never a parse failure.
        assert_eq!(sum(&s, &["l2_hits"]), 0);
    }

    #[test]
    fn the_hit_ratio_is_a_percentage_and_survives_an_empty_cache() {
        let s = parse_arcstats(FIXTURE);
        let ratio = hit_ratio(s["hits"], s["misses"]);
        assert!((ratio - 97.45).abs() < 0.01, "{ratio}");
        assert_eq!(hit_ratio(0, 0), 0.0);
        assert_eq!(hit_ratio(1, 0), 100.0);
    }

    #[test]
    fn the_drop_in_is_read_back_and_comments_are_ignored() {
        let text = tentanas_helper::arc_modprobe_file(8_589_934_592);
        assert_eq!(parse_modprobe_max(&text), Some(8_589_934_592));
        assert_eq!(parse_modprobe_max("# options zfs zfs_arc_max=1\n"), None);
        assert_eq!(parse_modprobe_max("options zfs zfs_arc_min=1\n"), None);
    }

    #[test]
    fn the_limit_source_names_what_is_in_force_now() {
        assert_eq!(limit_source(Some(8), Some(8)), "modprobe");
        // Someone lowered the running cap after boot: the drop-in is stale.
        assert_eq!(limit_source(Some(4), Some(8)), "runtime");
        assert_eq!(limit_source(Some(8), None), "runtime");
        assert_eq!(limit_source(Some(0), None), "default");
        assert_eq!(limit_source(None, None), "default");
    }
}
