// =============================================================================
// File: bus/lag_history.rs — persisted per-minute lag / DLQ-depth history
//       (SUM/tentabus/PLAN-UI-20260923.md B1b, owner decision 23.09: stored in
//       the database for 24 h, one sample per minute, survives a restart).
// =============================================================================
//
// Storage is the instance's own `tentabus.db` (`bus::db` step 2), next to
// `bus_groups`: what a node measured for an instance is node-local state, like
// the group rows it is keyed by, so it never enters the Sync Ledger and is
// removed together with the instance. One row per (org, group, topic) and one
// per (org, source topic with a DLQ) per sample — aggregated over partitions,
// which bounds a series at `RETENTION_MS / SAMPLE_INTERVAL` = 1 440 rows.
//
// The trend the UI shows ("rośnie od 25 min", "tempo czytania") is derived
// here from those rows, so every node answers from its own persisted history
// rather than from whatever happened since the dashboard was opened.
// =============================================================================

use std::time::Duration;

use anyhow::Result;

use crate::db::DbPool;

/// Spacing of the samples the background sampler writes.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);

/// How long samples are kept.
pub const RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

/// Two consecutive samples further apart than this are not one run: the
/// node (or the instance) was down in between, and nothing is known about
/// the lag during the gap.
pub const MAX_SAMPLE_GAP_MS: i64 = 3 * 60 * 1000;

/// Upper bound on the rows one trend computation reads — a full day of one
/// series.
const TREND_WINDOW_ROWS: i64 = RETENTION_MS / 60_000;

/// One consumer group's measured position on one topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSample {
    pub org_id: String,
    pub group_id: String,
    pub topic: String,
    pub lag_total: u64,
    pub committed_total: u64,
}

/// Records waiting in `__dlq.<topic>` (discarded ones excluded), keyed by the
/// SOURCE topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlqSample {
    pub org_id: String,
    pub topic: String,
    pub dlq_depth: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LagPoint {
    pub at_ms: i64,
    pub lag_total: u64,
    pub committed_total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSeries {
    pub group_id: String,
    pub topic: String,
    pub points: Vec<LagPoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DlqPoint {
    pub at_ms: i64,
    pub dlq_depth: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlqSeries {
    pub topic: String,
    pub points: Vec<DlqPoint>,
}

/// What `BusGroupStatsWire` reports next to the live lag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LagTrend {
    pub rising_since_ms: Option<i64>,
    pub consume_rate_per_min: Option<u64>,
}

fn to_sql(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn from_sql(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Writes one sampling round in a single transaction. `INSERT OR REPLACE`
/// keeps a retried round at the same timestamp idempotent.
pub fn record(db: &DbPool, at_ms: i64, groups: &[GroupSample], dlqs: &[DlqSample]) -> Result<()> {
    if groups.is_empty() && dlqs.is_empty() {
        return Ok(());
    }
    let mut conn = db.write()?;
    let tx = conn.transaction()?;
    {
        let mut insert_lag = tx.prepare_cached(
            "INSERT OR REPLACE INTO bus_lag_samples \
             (org_id, group_id, topic, sampled_at_ms, lag_total, committed_total) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for g in groups {
            insert_lag.execute(rusqlite::params![
                g.org_id,
                g.group_id,
                g.topic,
                at_ms,
                to_sql(g.lag_total),
                to_sql(g.committed_total),
            ])?;
        }
        let mut insert_dlq = tx.prepare_cached(
            "INSERT OR REPLACE INTO bus_dlq_samples (org_id, topic, sampled_at_ms, dlq_depth) \
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for d in dlqs {
            insert_dlq.execute(rusqlite::params![
                d.org_id,
                d.topic,
                at_ms,
                to_sql(d.dlq_depth)
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Deletes every sample older than `before_ms`; returns the number of rows
/// removed from both tables.
pub fn prune(db: &DbPool, before_ms: i64) -> Result<usize> {
    let conn = db.write()?;
    let lag = conn.execute(
        "DELETE FROM bus_lag_samples WHERE sampled_at_ms < ?1",
        rusqlite::params![before_ms],
    )?;
    let dlq = conn.execute(
        "DELETE FROM bus_dlq_samples WHERE sampled_at_ms < ?1",
        rusqlite::params![before_ms],
    )?;
    Ok(lag + dlq)
}

/// Removes an org's whole history (`BusService::purge_org`).
pub fn delete_org(db: &DbPool, org_id: &str) -> Result<usize> {
    let conn = db.write()?;
    let lag = conn.execute(
        "DELETE FROM bus_lag_samples WHERE org_id = ?1",
        rusqlite::params![org_id],
    )?;
    let dlq = conn.execute(
        "DELETE FROM bus_dlq_samples WHERE org_id = ?1",
        rusqlite::params![org_id],
    )?;
    Ok(lag + dlq)
}

/// Removes one topic's history (`BusService::delete_topic`), so a topic
/// re-created under the same name starts without its predecessor's trend.
pub fn delete_topic(db: &DbPool, org_id: &str, topic: &str) -> Result<usize> {
    let conn = db.write()?;
    let lag = conn.execute(
        "DELETE FROM bus_lag_samples WHERE org_id = ?1 AND topic = ?2",
        rusqlite::params![org_id, topic],
    )?;
    let dlq = conn.execute(
        "DELETE FROM bus_dlq_samples WHERE org_id = ?1 AND topic = ?2",
        rusqlite::params![org_id, topic],
    )?;
    Ok(lag + dlq)
}

/// Group series of `org_id` since `since_ms`, optionally narrowed to one
/// topic and/or one group. Points are oldest first.
pub fn group_series(
    db: &DbPool,
    org_id: &str,
    since_ms: i64,
    topic: Option<&str>,
    group: Option<&str>,
) -> Result<Vec<GroupSeries>> {
    let conn = db.read()?;
    let mut stmt = conn.prepare_cached(
        "SELECT group_id, topic, sampled_at_ms, lag_total, committed_total \
         FROM bus_lag_samples \
         WHERE org_id = ?1 AND sampled_at_ms >= ?2 \
           AND (?3 IS NULL OR topic = ?3) AND (?4 IS NULL OR group_id = ?4) \
         ORDER BY group_id, topic, sampled_at_ms",
    )?;
    let rows = stmt.query_map(rusqlite::params![org_id, since_ms, topic, group], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            LagPoint {
                at_ms: row.get(2)?,
                lag_total: from_sql(row.get(3)?),
                committed_total: from_sql(row.get(4)?),
            },
        ))
    })?;
    let mut out: Vec<GroupSeries> = Vec::new();
    for row in rows {
        let (group_id, topic, point) = row?;
        match out.last_mut() {
            Some(last) if last.group_id == group_id && last.topic == topic => {
                last.points.push(point)
            }
            _ => out.push(GroupSeries {
                group_id,
                topic,
                points: vec![point],
            }),
        }
    }
    Ok(out)
}

/// DLQ-depth series of `org_id` since `since_ms`, optionally one source
/// topic. Points are oldest first.
pub fn dlq_series(
    db: &DbPool,
    org_id: &str,
    since_ms: i64,
    topic: Option<&str>,
) -> Result<Vec<DlqSeries>> {
    let conn = db.read()?;
    let mut stmt = conn.prepare_cached(
        "SELECT topic, sampled_at_ms, dlq_depth FROM bus_dlq_samples \
         WHERE org_id = ?1 AND sampled_at_ms >= ?2 AND (?3 IS NULL OR topic = ?3) \
         ORDER BY topic, sampled_at_ms",
    )?;
    let rows = stmt.query_map(rusqlite::params![org_id, since_ms, topic], |row| {
        Ok((
            row.get::<_, String>(0)?,
            DlqPoint {
                at_ms: row.get(1)?,
                dlq_depth: from_sql(row.get(2)?),
            },
        ))
    })?;
    let mut out: Vec<DlqSeries> = Vec::new();
    for row in rows {
        let (topic, point) = row?;
        match out.last_mut() {
            Some(last) if last.topic == topic => last.points.push(point),
            _ => out.push(DlqSeries {
                topic,
                points: vec![point],
            }),
        }
    }
    Ok(out)
}

/// Trend state of one (group, topic) rebuilt from its persisted samples —
/// read once per series when the sampler starts; every later sample is
/// folded in with `TrendState::push`, without re-reading the table. `None`
/// for a series with no samples.
pub fn group_trend_state(
    db: &DbPool,
    org_id: &str,
    group_id: &str,
    topic: &str,
) -> Result<Option<TrendState>> {
    let conn = db.read()?;
    let mut stmt = conn.prepare_cached(
        "SELECT sampled_at_ms, lag_total, committed_total FROM bus_lag_samples \
         WHERE org_id = ?1 AND group_id = ?2 AND topic = ?3 \
         ORDER BY sampled_at_ms DESC LIMIT ?4",
    )?;
    let mut points = stmt
        .query_map(
            rusqlite::params![org_id, group_id, topic, TREND_WINDOW_ROWS],
            |row| {
                Ok(LagPoint {
                    at_ms: row.get(0)?,
                    lag_total: from_sql(row.get(1)?),
                    committed_total: from_sql(row.get(2)?),
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    points.reverse();
    Ok(TrendState::from_points(&points))
}

/// Everything the trend needs from a series, kept incrementally: the two
/// newest samples (for the rate) and the `anchor` — the sample the current
/// growth started from.
///
/// `rising_since_ms`: the newest samples form a run in which the lag never
/// went down and no two neighbours are more than `MAX_SAMPLE_GAP_MS` apart;
/// if the lag is positive and grew over that run, the answer is the sample
/// the growth started from (a flat stretch before the first increase does
/// not count). A drop in the last step ends the run, so a group that is
/// catching up is never reported as "rising".
///
/// `consume_rate_per_min`: what the group acknowledged between the two
/// newest samples, scaled to 60 s. `None` across a gap or when the committed
/// total went backwards (an offset reset is not negative throughput).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrendState {
    last: LagPoint,
    prev: Option<LagPoint>,
    anchor: LagPoint,
}

impl TrendState {
    pub fn start(point: LagPoint) -> Self {
        Self {
            last: point,
            prev: None,
            anchor: point,
        }
    }

    /// Folds `points` (oldest first) into a state.
    pub fn from_points(points: &[LagPoint]) -> Option<Self> {
        let (first, rest) = points.split_first()?;
        let mut state = Self::start(*first);
        for p in rest {
            state.push(*p);
        }
        Some(state)
    }

    /// Adds the next sample. A sample not newer than the last one (a retried
    /// round) changes nothing.
    pub fn push(&mut self, point: LagPoint) {
        if point.at_ms <= self.last.at_ms {
            return;
        }
        let continues_run = point.at_ms - self.last.at_ms <= MAX_SAMPLE_GAP_MS
            && point.lag_total >= self.last.lag_total;
        // A new run starts at `point`; inside a run the anchor follows the
        // flat stretch it began with, so it ends on the last sample before
        // the first increase.
        if !continues_run || self.anchor.lag_total == point.lag_total {
            self.anchor = point;
        }
        self.prev = Some(self.last);
        self.last = point;
    }

    pub fn trend(&self) -> LagTrend {
        let consume_rate_per_min = self.prev.and_then(|prev| {
            let step_ms = self.last.at_ms - prev.at_ms;
            (step_ms > 0
                && step_ms <= MAX_SAMPLE_GAP_MS
                && self.last.committed_total >= prev.committed_total)
                .then(|| {
                    let acked = u128::from(self.last.committed_total - prev.committed_total);
                    u64::try_from(acked * 60_000 / step_ms as u128).unwrap_or(u64::MAX)
                })
        });
        let rising_since_ms = (self.last.lag_total > 0
            && self.anchor.lag_total < self.last.lag_total)
            .then_some(self.anchor.at_ms);
        LagTrend {
            rising_since_ms,
            consume_rate_per_min,
        }
    }
}

/// Trend of samples ordered oldest first (see `TrendState`).
pub fn trend(points: &[LagPoint]) -> LagTrend {
    TrendState::from_points(points)
        .map(|s| s.trend())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn points(lags: &[(i64, u64, u64)]) -> Vec<LagPoint> {
        lags.iter()
            .map(|&(minute, lag_total, committed_total)| LagPoint {
                at_ms: minute * MIN,
                lag_total,
                committed_total,
            })
            .collect()
    }

    fn local_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::bus::db::migrate(&conn).unwrap();
        std::sync::Arc::new(crate::db::Db::from_connection(conn))
    }

    /// Node-local by design: the history tables live in the instance's own
    /// `tentabus.db`, and no Sync Ledger descriptor may name them.
    #[test]
    fn history_tables_are_not_synced() {
        for d in crate::sync::core_registry::CORE_SYNC_DESCRIPTORS {
            assert!(
                !matches!(d.table_name, "bus_lag_samples" | "bus_dlq_samples"),
                "{} must stay node-local",
                d.table_name
            );
        }
    }

    #[test]
    fn fewer_than_two_samples_have_no_trend() {
        assert_eq!(trend(&[]), LagTrend::default());
        assert_eq!(trend(&points(&[(1, 50, 0)])), LagTrend::default());
    }

    #[test]
    fn a_growing_lag_reports_where_the_growth_started() {
        // Flat at 10 until minute 3, then growing: the growth starts at 3.
        let t = trend(&points(&[
            (0, 10, 0),
            (1, 10, 0),
            (3, 10, 0),
            (4, 30, 0),
            (5, 70, 0),
        ]));
        assert_eq!(t.rising_since_ms, Some(3 * MIN));
    }

    #[test]
    fn a_flat_minute_inside_the_growth_does_not_reset_it() {
        let t = trend(&points(&[(0, 5, 0), (1, 20, 0), (2, 20, 0), (3, 40, 0)]));
        assert_eq!(t.rising_since_ms, Some(0));
    }

    #[test]
    fn a_falling_last_step_is_not_rising() {
        let t = trend(&points(&[(0, 5, 0), (1, 50, 0), (2, 40, 50)]));
        assert_eq!(t.rising_since_ms, None);
    }

    #[test]
    fn a_flat_lag_is_not_rising() {
        let t = trend(&points(&[(0, 40, 0), (1, 40, 0), (2, 40, 0)]));
        assert_eq!(t.rising_since_ms, None);
    }

    #[test]
    fn zero_lag_is_never_rising() {
        let t = trend(&points(&[(0, 0, 0), (1, 0, 10), (2, 0, 20)]));
        assert_eq!(t.rising_since_ms, None);
    }

    #[test]
    fn a_paused_group_with_incoming_traffic_is_rising() {
        // Paused: nothing is acknowledged, the lag grows with every publish.
        let t = trend(&points(&[(0, 100, 900), (1, 180, 900), (2, 260, 900)]));
        assert_eq!(t.rising_since_ms, Some(0));
        assert_eq!(t.consume_rate_per_min, Some(0));
    }

    #[test]
    fn a_gap_in_the_samples_ends_the_run() {
        // Minutes 0-1 rising, a 10-minute gap, then rising again from 11.
        let t = trend(&points(&[(0, 10, 0), (1, 20, 0), (11, 30, 0), (12, 45, 0)]));
        assert_eq!(t.rising_since_ms, Some(11 * MIN));
        let across_gap = trend(&points(&[(0, 10, 0), (10, 30, 0)]));
        assert_eq!(across_gap.rising_since_ms, None);
        assert_eq!(across_gap.consume_rate_per_min, None);
    }

    /// Pushing samples one at a time lands on the same trend as folding the
    /// whole prefix — the sampler relies on it instead of re-reading rows.
    #[test]
    fn incremental_state_matches_every_prefix() {
        let series = points(&[
            (0, 10, 0),
            (1, 10, 5),
            (2, 30, 5),
            (3, 30, 9),
            (4, 20, 30),
            (10, 25, 30),
            (11, 60, 31),
            (12, 60, 40),
        ]);
        let mut state = TrendState::start(series[0]);
        for n in 2..=series.len() {
            state.push(series[n - 1]);
            assert_eq!(
                state.trend(),
                reference_trend(&series[..n]),
                "prefix of {n}"
            );
        }
        state.push(series[3]);
        assert_eq!(
            state.trend(),
            reference_trend(&series),
            "an older sample is ignored"
        );
    }

    /// The whole-series definition `TrendState` must agree with: walk back
    /// over the non-decreasing, gap-free run ending at the newest sample,
    /// then skip its leading flat stretch.
    fn reference_trend(points: &[LagPoint]) -> LagTrend {
        let n = points.len();
        if n < 2 {
            return LagTrend::default();
        }
        let (last, prev) = (points[n - 1], points[n - 2]);
        let step = last.at_ms - prev.at_ms;
        let consume_rate_per_min =
            (step > 0 && step <= MAX_SAMPLE_GAP_MS && last.committed_total >= prev.committed_total)
                .then(|| (last.committed_total - prev.committed_total) * 60_000 / step as u64);
        let mut start = n - 1;
        while start > 0
            && points[start].at_ms - points[start - 1].at_ms <= MAX_SAMPLE_GAP_MS
            && points[start - 1].lag_total <= points[start].lag_total
        {
            start -= 1;
        }
        while start < n - 1 && points[start + 1].lag_total == points[start].lag_total {
            start += 1;
        }
        LagTrend {
            rising_since_ms: (last.lag_total > 0 && points[start].lag_total < last.lag_total)
                .then_some(points[start].at_ms),
            consume_rate_per_min,
        }
    }

    #[test]
    fn consume_rate_is_scaled_to_one_minute() {
        let t = trend(&[
            LagPoint {
                at_ms: 0,
                lag_total: 5,
                committed_total: 1_000,
            },
            LagPoint {
                at_ms: 30_000,
                lag_total: 5,
                committed_total: 1_200,
            },
        ]);
        assert_eq!(t.consume_rate_per_min, Some(400));
    }

    #[test]
    fn an_offset_reset_backwards_has_no_rate() {
        let t = trend(&points(&[(0, 10, 500), (1, 400, 100)]));
        assert_eq!(t.consume_rate_per_min, None);
    }

    #[test]
    fn record_prune_and_read_back_series() {
        let db = local_db();
        let g = |group: &str, topic: &str, lag: u64, committed: u64| GroupSample {
            org_id: "org-1".to_string(),
            group_id: group.to_string(),
            topic: topic.to_string(),
            lag_total: lag,
            committed_total: committed,
        };
        let d = |topic: &str, depth: u64| DlqSample {
            org_id: "org-1".to_string(),
            topic: topic.to_string(),
            dlq_depth: depth,
        };
        record(
            &db,
            MIN,
            &[g("a", "t1", 1, 10), g("b", "t2", 5, 0)],
            &[d("t1", 2)],
        )
        .unwrap();
        record(&db, 2 * MIN, &[g("a", "t1", 3, 12)], &[d("t1", 4)]).unwrap();
        // Retrying the same round is idempotent.
        record(&db, 2 * MIN, &[g("a", "t1", 3, 12)], &[d("t1", 4)]).unwrap();

        let all = group_series(&db, "org-1", 0, None, None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].group_id, "a");
        assert_eq!(
            all[0]
                .points
                .iter()
                .map(|p| p.lag_total)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        let only_t2 = group_series(&db, "org-1", 0, Some("t2"), None).unwrap();
        assert_eq!(only_t2.len(), 1);
        assert_eq!(only_t2[0].group_id, "b");
        let only_a = group_series(&db, "org-1", 0, None, Some("a")).unwrap();
        assert_eq!(only_a.len(), 1);
        assert!(group_series(&db, "org-2", 0, None, None)
            .unwrap()
            .is_empty());

        let dlq = dlq_series(&db, "org-1", 0, Some("t1")).unwrap();
        assert_eq!(
            dlq[0]
                .points
                .iter()
                .map(|p| p.dlq_depth)
                .collect::<Vec<_>>(),
            vec![2, 4]
        );

        assert_eq!(
            group_trend_state(&db, "org-1", "a", "t1")
                .unwrap()
                .map(|s| s.trend().rising_since_ms),
            Some(Some(MIN))
        );

        // Everything of minute 1 goes; minute 2 stays.
        assert_eq!(prune(&db, 2 * MIN).unwrap(), 3);
        let after = group_series(&db, "org-1", 0, None, None).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].points.len(), 1);

        assert_eq!(delete_topic(&db, "org-1", "t1").unwrap(), 2);
        assert!(group_series(&db, "org-1", 0, None, None)
            .unwrap()
            .is_empty());
        record(&db, 3 * MIN, &[g("a", "t1", 1, 1)], &[]).unwrap();
        assert_eq!(delete_org(&db, "org-1").unwrap(), 1);
    }
}
