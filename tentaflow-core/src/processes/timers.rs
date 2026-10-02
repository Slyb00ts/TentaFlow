// ============ File: timers.rs — durable process timer slot arithmetic and transition planning ============

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Days, Duration, LocalResult, NaiveDate, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::json;
use tentaflow_protocol::processes::{ProcessTimerKind, ProcessTimerSpec, ProcessTimerStatus};
use uuid::Uuid;

use super::repository::{
    self, CancelledJobClaim, PlannedEvent, ProcessTimer, RuntimePlan, TimerSnapshot, TimerUpdate,
};
use super::runtime::{self, StartCause};
use crate::db::DbPool;
use crate::project_studio::schedules::{compute_next_run, parse_timezone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerAdvanceMode {
    Fire,
    Restore,
}

#[derive(Debug, Clone)]
pub struct TimerAdvance {
    pub selected_occurrence: u64,
    pub planned_due_at_ms: i64,
    pub skipped_count: u64,
    pub next_occurrence: u64,
    pub next_due_at_ms: Option<i64>,
    pub status: ProcessTimerStatus,
}

fn instant(at_ms: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_millis(at_ms)
        .context("timer instant exceeds the supported date horizon")
}

fn timezone(name: &str) -> Result<Tz> {
    ensure!(
        !name.trim().is_empty(),
        "process timers require an explicit IANA timezone"
    );
    parse_timezone(name)
}

fn checked_occurrence(occurrence: u64) -> Result<u64> {
    ensure!(occurrence > 0, "timer occurrence must be positive");
    i64::try_from(occurrence).context("timer occurrence exceeds SQLite INTEGER range")?;
    Ok(occurrence)
}

fn local_date(at: DateTime<Utc>, tz: Tz) -> Result<NaiveDate> {
    let offset = tz
        .offset_from_utc_datetime(&at.naive_utc())
        .fix()
        .local_minus_utc();
    at.naive_utc()
        .checked_add_signed(Duration::seconds(i64::from(offset)))
        .map(|local| local.date())
        .context("timer timezone exceeds the supported calendar horizon")
}

fn elapsed_due(anchor_at_ms: i64, seconds: u32, occurrence: u64) -> Result<i64> {
    checked_occurrence(occurrence)?;
    let elapsed = i128::from(seconds)
        .checked_mul(1000)
        .and_then(|period| period.checked_mul(i128::from(occurrence)))
        .context("timer interval arithmetic overflow")?;
    let due = i64::try_from(i128::from(anchor_at_ms) + elapsed)
        .context("timer due instant exceeds SQLite INTEGER range")?;
    instant(due)?;
    Ok(due)
}

fn daily_after(hour: u8, minute: u8, zone: &str, cursor: DateTime<Utc>) -> Result<i64> {
    let tz = timezone(zone)?;
    ensure!(
        hour < 24 && minute < 60,
        "daily timer wall-clock time is invalid"
    );
    local_date(cursor, tz)?
        .checked_add_days(Days::new(3))
        .context("daily timer exceeds the supported calendar horizon")?;
    compute_next_run("cron", &format!("{minute} {hour} * * *"), zone, cursor)?
        .map(|next| next.timestamp_millis())
        .context("daily timer has no next due instant")
}

pub fn resolve_timer_due(
    spec: &ProcessTimerSpec,
    zone: &str,
    kind: ProcessTimerKind,
    anchor_at_ms: i64,
) -> Result<i64> {
    timezone(zone)?;
    let anchor = instant(anchor_at_ms)?;
    match spec {
        ProcessTimerSpec::Date { at } => {
            let at = DateTime::parse_from_rfc3339(at)
                .context("timer Date requires an RFC3339 instant")?;
            ensure!(
                at.timestamp_subsec_nanos() % 1_000_000 == 0,
                "timer Date supports millisecond precision"
            );
            let due = at.timestamp_millis();
            instant(due)?;
            ensure!(
                kind != ProcessTimerKind::Start || due > anchor_at_ms,
                "start timer Date must be in the future at publication"
            );
            Ok(due)
        }
        ProcessTimerSpec::Duration { seconds } => {
            ensure!(
                (1..=31_536_000).contains(seconds),
                "timer Duration is outside the supported range"
            );
            elapsed_due(anchor_at_ms, *seconds, 1)
        }
        ProcessTimerSpec::Cycle {
            seconds,
            total_firings,
        } => {
            ensure!(
                kind == ProcessTimerKind::Start,
                "Cycle is only supported on timer starts"
            );
            ensure!(
                (300..=31_536_000).contains(seconds),
                "timer Cycle is outside the supported range"
            );
            ensure!(
                total_firings.is_none_or(|count| count > 0),
                "timer Cycle count must be positive"
            );
            elapsed_due(anchor_at_ms, *seconds, 1)
        }
        ProcessTimerSpec::Daily {
            hour,
            minute,
            total_firings,
        } => {
            ensure!(
                kind == ProcessTimerKind::Start,
                "Daily is only supported on timer starts"
            );
            ensure!(
                total_firings.is_none_or(|count| count > 0),
                "daily timer count must be positive"
            );
            daily_after(*hour, *minute, zone, anchor)
        }
    }
}

fn daily_date_due(date: NaiveDate, hour: u8, minute: u8, zone: &str) -> Result<i64> {
    let tz = timezone(zone)?;
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .context("daily timer midnight is invalid")?;
    let start = match tz.from_local_datetime(&midnight) {
        LocalResult::Single(at) | LocalResult::Ambiguous(at, _) => at,
        LocalResult::None => {
            let shifted = midnight
                .checked_add_signed(Duration::hours(1))
                .context("daily timer midnight exceeds the supported calendar horizon")?;
            match tz.from_local_datetime(&shifted) {
                LocalResult::Single(at) | LocalResult::Ambiguous(at, _) => at,
                LocalResult::None => {
                    anyhow::bail!("daily timer date has no supported midnight in '{zone}'")
                }
            }
        }
    };
    let cursor = start
        .with_timezone(&Utc)
        .checked_sub_signed(Duration::milliseconds(1))
        .context("daily timer cursor exceeds the supported date horizon")?;
    let due = daily_after(hour, minute, zone, cursor)?;
    ensure!(
        local_date(instant(due)?, tz)? == date,
        "daily timer date has no supported firing instant in '{zone}'"
    );
    Ok(due)
}

fn due_for_occurrence(timer: &ProcessTimer, occurrence: u64) -> Result<i64> {
    checked_occurrence(occurrence)?;
    match &timer.rule {
        ProcessTimerSpec::Cycle { seconds, .. } => {
            resolve_timer_due(
                &timer.rule,
                &timer.timezone,
                timer.kind.clone(),
                timer.anchor_at_ms,
            )?;
            elapsed_due(timer.anchor_at_ms, *seconds, occurrence)
        }
        ProcessTimerSpec::Daily { hour, minute, .. } => {
            let first = resolve_timer_due(
                &timer.rule,
                &timer.timezone,
                timer.kind.clone(),
                timer.anchor_at_ms,
            )?;
            let first_date = local_date(instant(first)?, timezone(&timer.timezone)?)?;
            let date = first_date
                .checked_add_days(Days::new(occurrence - 1))
                .context("daily timer occurrence exceeds the supported calendar horizon")?;
            daily_date_due(date, *hour, *minute, &timer.timezone)
        }
        _ => {
            ensure!(
                occurrence == 1,
                "one-shot timer has more than one occurrence"
            );
            resolve_timer_due(
                &timer.rule,
                &timer.timezone,
                timer.kind.clone(),
                timer.anchor_at_ms,
            )
        }
    }
}

pub fn next_timer_occurrence(
    timer: &ProcessTimer,
    at_ms: i64,
    mode: TimerAdvanceMode,
) -> Result<TimerAdvance> {
    instant(at_ms)?;
    checked_occurrence(timer.occurrence)?;
    let due = timer
        .due_at_ms
        .context("timer has no pending due instant")?;
    ensure!(
        due_for_occurrence(timer, timer.occurrence)? == due,
        "persisted timer due does not match its anchored occurrence"
    );
    let total = match &timer.rule {
        ProcessTimerSpec::Cycle { total_firings, .. }
        | ProcessTimerSpec::Daily { total_firings, .. } => *total_firings,
        _ => Some(1),
    };
    ensure!(
        timer.total_firings == total,
        "timer count does not match its pinned rule"
    );
    ensure!(
        total.is_none_or(|count| count > 0 && timer.occurrence <= u64::from(count)),
        "timer occurrence exceeds its finite count"
    );
    if due > at_ms {
        ensure!(mode == TimerAdvanceMode::Restore, "timer is not due");
        return Ok(TimerAdvance {
            selected_occurrence: timer.occurrence,
            planned_due_at_ms: due,
            skipped_count: 0,
            next_occurrence: timer.occurrence,
            next_due_at_ms: Some(due),
            status: ProcessTimerStatus::Pending,
        });
    }
    if !matches!(
        &timer.rule,
        ProcessTimerSpec::Cycle { .. } | ProcessTimerSpec::Daily { .. }
    ) {
        return Ok(TimerAdvance {
            selected_occurrence: 1,
            planned_due_at_ms: due,
            skipped_count: u64::from(mode == TimerAdvanceMode::Restore),
            next_occurrence: 1,
            next_due_at_ms: None,
            status: if mode == TimerAdvanceMode::Fire {
                ProcessTimerStatus::Fired
            } else {
                ProcessTimerStatus::Missed
            },
        });
    }
    let latest = match &timer.rule {
        ProcessTimerSpec::Cycle { seconds, .. } => {
            let slots = (i128::from(at_ms) - i128::from(timer.anchor_at_ms))
                / (i128::from(*seconds) * 1000);
            u64::try_from(slots)
                .context("timer elapsed slots exceed the supported counter range")?
        }
        ProcessTimerSpec::Daily { .. } => {
            let tz = timezone(&timer.timezone)?;
            let first = resolve_timer_due(
                &timer.rule,
                &timer.timezone,
                timer.kind.clone(),
                timer.anchor_at_ms,
            )?;
            let first_date = local_date(instant(first)?, tz)?;
            let current_date = local_date(instant(at_ms)?, tz)?;
            let days = (current_date - first_date).num_days();
            let guess = u64::try_from(days)
                .context("daily timer precedes its first slot")?
                .checked_add(1)
                .context("daily timer slot overflow")?;
            let guess = total.map_or(guess, |count| guess.min(u64::from(count)));
            if due_for_occurrence(timer, guess)? > at_ms {
                guess
                    .checked_sub(1)
                    .context("daily timer has no due slot")?
            } else {
                guess
            }
        }
        _ => anyhow::bail!("timer rule does not have repeated slots"),
    };
    let selected = total.map_or(latest, |count| latest.min(u64::from(count)));
    checked_occurrence(selected)?;
    ensure!(
        selected >= timer.occurrence,
        "timer selected slot precedes its pending occurrence"
    );
    let planned_due_at_ms = due_for_occurrence(timer, selected)?;
    let skipped_count = selected - timer.occurrence + u64::from(mode == TimerAdvanceMode::Restore);
    if total.is_some_and(|count| selected == u64::from(count)) {
        return Ok(TimerAdvance {
            selected_occurrence: selected,
            planned_due_at_ms,
            skipped_count,
            next_occurrence: selected,
            next_due_at_ms: None,
            status: ProcessTimerStatus::Fired,
        });
    }
    let next_occurrence = checked_occurrence(
        selected
            .checked_add(1)
            .context("timer occurrence overflow")?,
    )?;
    let next_due = due_for_occurrence(timer, next_occurrence)?;
    ensure!(
        next_due > at_ms,
        "next timer occurrence is not strictly in the future"
    );
    Ok(TimerAdvance {
        selected_occurrence: selected,
        planned_due_at_ms,
        skipped_count,
        next_occurrence,
        next_due_at_ms: Some(next_due),
        status: ProcessTimerStatus::Pending,
    })
}

pub fn plan_timer_fire(snapshot: &TimerSnapshot, at_ms: i64) -> Result<RuntimePlan> {
    let timer = match snapshot {
        TimerSnapshot::Start { timer, .. }
        | TimerSnapshot::Catch { timer, .. }
        | TimerSnapshot::Boundary { timer, .. } => timer,
    };
    let advance = next_timer_occurrence(timer, at_ms, TimerAdvanceMode::Fire)?;
    let mut plan = match snapshot {
        TimerSnapshot::Start {
            actor,
            timer,
            version,
        } => {
            let instance_id = Uuid::new_v4().to_string();
            runtime::plan_start(
                &version.model,
                &instance_id,
                actor,
                &timer.definition_id,
                timer.version,
                serde_json::to_value(&version.model.variables)?,
                StartCause::Timer {
                    timer_id: timer.timer_id.clone(),
                    occurrence: advance.selected_occurrence,
                },
                at_ms,
            )?
        }
        TimerSnapshot::Catch {
            timer, snapshot, ..
        } => runtime::plan_timer_catch(snapshot, timer, at_ms)?,
        TimerSnapshot::Boundary {
            timer, snapshot, ..
        } => runtime::plan_timer_boundary(snapshot, timer, at_ms)?,
    };
    let skipped_from = (advance.skipped_count > 0).then_some(timer.occurrence);
    let skipped_through = (advance.skipped_count > 0).then(|| advance.selected_occurrence - 1);
    let mut event = PlannedEvent {
        kind: "timer_fired".into(), node_id: Some(timer.node_id.clone()),
        data: json!({"timer_id":timer.timer_id,"kind":timer.kind,"occurrence":advance.selected_occurrence,
            "planned_due_at_ms":advance.planned_due_at_ms,"fired_at_ms":at_ms,"skipped_count":advance.skipped_count,
            "skipped_from_occurrence":skipped_from,"skipped_through_occurrence":skipped_through}),
    };
    if let TimerSnapshot::Boundary { snapshot, .. } = snapshot {
        let node = snapshot
            .model
            .nodes
            .iter()
            .find(|node| node.id == timer.node_id)
            .context("boundary node is missing")?;
        let tentaflow_protocol::processes::ProcessNodeKind::BoundaryTimer {
            attached_to_id,
            cancel_activity,
            ..
        } = &node.kind
        else {
            anyhow::bail!("boundary timer references another node kind");
        };
        let fields = event
            .data
            .as_object_mut()
            .context("timer event must be an object")?;
        fields.insert("attached_to_id".into(), json!(attached_to_id));
        fields.insert("attached_token_id".into(), json!(timer.token_id));
        fields.insert("cancel_activity".into(), json!(cancel_activity));
        fields.insert(
            "cancelled_user_task_ids".into(),
            json!(plan.cancel_user_task_ids),
        );
        fields.insert("cancelled_job_ids".into(), json!(plan.cancel_job_ids));
        plan.events.insert(0, event);
    } else {
        plan.events.push(event);
    }
    plan.timer_updates.push(TimerUpdate {
        timer_id: timer.timer_id.clone(),
        expected_revision: timer.revision,
        fired_occurrence: Some(advance.selected_occurrence),
        occurrence: advance.next_occurrence,
        due_at_ms: advance.next_due_at_ms,
        status: advance.status,
        last_reason: None,
        next_check_at_ms: advance.next_due_at_ms.unwrap_or(at_ms),
    });
    Ok(plan)
}

#[derive(Debug)]
pub struct TimerDrainOutcome {
    pub fired: u32,
    pub cancelled_claims: Vec<CancelledJobClaim>,
    pub completion: Result<()>,
}

pub fn drain_due(pool: &DbPool, at_ms: i64) -> TimerDrainOutcome {
    let mut drained = TimerDrainOutcome {
        fired: 0,
        cancelled_claims: Vec::new(),
        completion: Ok(()),
    };
    let candidates = match repository::due_timers(pool, at_ms, 32) {
        Ok(candidates) => candidates,
        Err(error) => {
            drained.completion = Err(error);
            return drained;
        }
    };
    for candidate in candidates {
        let outcome = (|| {
            let snapshot = repository::timer_snapshot(pool, &candidate)?;
            let plan = plan_timer_fire(&snapshot, at_ms)?;
            let (actor, revision) = match &snapshot {
                TimerSnapshot::Start { actor, .. } => (actor, None),
                TimerSnapshot::Catch {
                    actor, snapshot, ..
                }
                | TimerSnapshot::Boundary {
                    actor, snapshot, ..
                } => (actor, Some(snapshot.instance.revision)),
            };
            repository::fire_timer(pool, &candidate, actor, revision, &plan, at_ms)
        })();
        match outcome {
            Ok(Some(committed)) => {
                drained.fired += 1;
                drained.cancelled_claims.extend(committed.cancelled_claims);
            }
            Ok(None) => {}
            Err(error) => {
                let reason = format!("{error:#}");
                tracing::warn!(
                    timer_id = candidate.timer_id,
                    reason,
                    "process timer could not fire"
                );
                let recorded = if error
                    .downcast_ref::<repository::ProcessAuthorityDenied>()
                    .is_some()
                {
                    repository::record_timer_blocked(pool, &candidate, &reason, at_ms)
                } else {
                    repository::record_timer_failed(pool, &candidate, &reason, at_ms)
                };
                if let Err(error) = recorded {
                    // Earlier candidates have committed; their cancellation signals must survive this failure.
                    drained.completion = Err(error);
                    break;
                }
            }
        }
    }
    drained
}

#[cfg(test)]
mod tests {
    use super::super::runtime::test_support::*;
    use super::*;
    use std::collections::BTreeMap;
    use tentaflow_protocol::processes::{
        ActivityVerification, ProcessInstance, ProcessInstanceStatus, ProcessModel, ProcessNode,
        ProcessNodeKind, ProcessUserTaskStatus,
    };

    fn at(text: &str) -> i64 {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .timestamp_millis()
    }

    fn timer(rule: ProcessTimerSpec, zone: &str, anchor: i64) -> ProcessTimer {
        let total_firings = match &rule {
            ProcessTimerSpec::Cycle { total_firings, .. }
            | ProcessTimerSpec::Daily { total_firings, .. } => *total_firings,
            _ => Some(1),
        };
        let due = resolve_timer_due(&rule, zone, ProcessTimerKind::Start, anchor).unwrap();
        ProcessTimer {
            timer_id: Uuid::new_v4().to_string(),
            org_id: Uuid::new_v4().to_string(),
            definition_id: Uuid::new_v4().to_string(),
            version: 1,
            node_id: "Start_1".into(),
            kind: ProcessTimerKind::Start,
            instance_id: None,
            token_id: None,
            rule,
            timezone: zone.into(),
            anchor_at_ms: anchor,
            due_at_ms: Some(due),
            occurrence: 1,
            total_firings,
            revision: 1,
            status: ProcessTimerStatus::Pending,
            last_reason: None,
            next_check_at_ms: due,
            created_at_ms: anchor,
            updated_at_ms: anchor,
        }
    }

    fn catch_model(rule: ProcessTimerSpec) -> ProcessModel {
        let mut model = super::super::model::starter_model();
        model.timer_timezone = Some("Europe/Warsaw".into());
        model.nodes.insert(
            1,
            ProcessNode {
                id: "Wait".into(),
                name: "Wait for the due instant".into(),
                kind: ProcessNodeKind::TimerCatch { timer: rule },
            },
        );
        model.sequence_flows = vec![
            edge("ToWait", "Start_1", "Wait"),
            edge("ToEnd", "Wait", "End_1"),
        ];
        model
    }

    fn start_at(fixture: &Fixture, model: &ProcessModel, at_ms: i64) -> ProcessInstance {
        let version = publish_model(fixture, model);
        let id = Uuid::new_v4().to_string();
        let variables = serde_json::to_value(&model.variables).unwrap();
        let plan = runtime::plan_start(
            model,
            &id,
            &fixture.owner,
            &version.definition_id,
            version.version,
            variables.clone(),
            StartCause::Manual,
            at_ms,
        )
        .unwrap();
        repository::start_instance(
            &fixture.db,
            &fixture.owner,
            &stamp("timed manual start"),
            &id,
            &version.definition_id,
            version.version,
            &variables,
            &plan,
            at_ms,
        )
        .unwrap()
    }

    #[test]
    fn due_rules_require_explicit_timezone_and_preserve_elapsed_and_absolute_instants() {
        let anchor = at("2026-03-29T00:30:00Z");
        let date = ProcessTimerSpec::Date {
            at: "2026-03-29T03:30:00+02:00".into(),
        };
        assert_eq!(
            resolve_timer_due(&date, "Europe/Warsaw", ProcessTimerKind::Start, anchor).unwrap(),
            anchor + 3_600_000
        );
        assert!(resolve_timer_due(&date, "", ProcessTimerKind::Catch, anchor).is_err());
        assert!(resolve_timer_due(&date, "Mars/Unknown", ProcessTimerKind::Catch, anchor).is_err());
        assert!(
            resolve_timer_due(&date, "UTC", ProcessTimerKind::Start, anchor + 3_600_000).is_err()
        );
        assert_eq!(
            resolve_timer_due(&date, "UTC", ProcessTimerKind::Catch, anchor + 7_200_000).unwrap(),
            anchor + 3_600_000
        );
        assert!(resolve_timer_due(
            &ProcessTimerSpec::Date {
                at: "2026-03-29T01:30:00.0001Z".into()
            },
            "UTC",
            ProcessTimerKind::Catch,
            anchor
        )
        .is_err());
        assert_eq!(
            resolve_timer_due(
                &ProcessTimerSpec::Duration { seconds: 7200 },
                "Europe/Warsaw",
                ProcessTimerKind::Catch,
                anchor
            )
            .unwrap(),
            anchor + 7_200_000
        );
        assert!(resolve_timer_due(
            &ProcessTimerSpec::Duration { seconds: 0 },
            "UTC",
            ProcessTimerKind::Catch,
            anchor
        )
        .is_err());
        assert!(resolve_timer_due(
            &ProcessTimerSpec::Cycle {
                seconds: 300,
                total_firings: None
            },
            "UTC",
            ProcessTimerKind::Catch,
            anchor
        )
        .is_err());
        assert!(resolve_timer_due(
            &ProcessTimerSpec::Duration { seconds: 1 },
            "UTC",
            ProcessTimerKind::Catch,
            DateTime::<Utc>::MAX_UTC.timestamp_millis()
        )
        .is_err());
    }

    #[test]
    fn repeated_cycle_coalesces_latest_due_slot_and_finite_count_includes_skips() {
        let anchor = at("2026-01-01T00:00:00Z");
        let finite = timer(
            ProcessTimerSpec::Cycle {
                seconds: 300,
                total_firings: Some(3),
            },
            "UTC",
            anchor,
        );
        let advance =
            next_timer_occurrence(&finite, anchor + 1_005_000, TimerAdvanceMode::Fire).unwrap();
        assert_eq!(advance.selected_occurrence, 3);
        assert_eq!(advance.planned_due_at_ms, anchor + 900_000);
        assert_eq!(advance.skipped_count, 2);
        assert_eq!(advance.next_occurrence, 3);
        assert_eq!(advance.next_due_at_ms, None);
        assert_eq!(advance.status, ProcessTimerStatus::Fired);
        let unlimited = timer(
            ProcessTimerSpec::Cycle {
                seconds: 300,
                total_firings: None,
            },
            "UTC",
            anchor,
        );
        let advance =
            next_timer_occurrence(&unlimited, anchor + 1_005_000, TimerAdvanceMode::Fire).unwrap();
        assert_eq!(advance.next_occurrence, 4);
        assert_eq!(advance.next_due_at_ms, Some(anchor + 1_200_000));
        let restore =
            next_timer_occurrence(&unlimited, anchor + 1_005_000, TimerAdvanceMode::Restore)
                .unwrap();
        assert_eq!(restore.skipped_count, 3);
        assert_eq!(restore.next_due_at_ms, Some(anchor + 1_200_000));
        assert!(elapsed_due(anchor, 300, u64::MAX).is_err());
        assert!(next_timer_occurrence(
            &unlimited,
            DateTime::<Utc>::MAX_UTC.timestamp_millis(),
            TimerAdvanceMode::Fire
        )
        .is_err());
        let one_shot = timer(ProcessTimerSpec::Duration { seconds: 30 }, "UTC", anchor);
        assert_eq!(
            next_timer_occurrence(&one_shot, anchor + 60_000, TimerAdvanceMode::Restore)
                .unwrap()
                .status,
            ProcessTimerStatus::Missed
        );
    }

    #[test]
    fn daily_slots_reuse_actual_gap_fold_policy_and_calendar_coalescing() {
        let spring = timer(
            ProcessTimerSpec::Daily {
                hour: 2,
                minute: 30,
                total_firings: None,
            },
            "Europe/Warsaw",
            at("2026-03-27T23:00:00Z"),
        );
        assert_eq!(spring.due_at_ms, Some(at("2026-03-28T01:30:00Z")));
        assert_eq!(
            due_for_occurrence(&spring, 2).unwrap(),
            at("2026-03-29T01:30:00Z")
        );
        assert_eq!(
            due_for_occurrence(&spring, 3).unwrap(),
            at("2026-03-30T00:30:00Z")
        );
        let fired =
            next_timer_occurrence(&spring, at("2026-03-30T10:00:00Z"), TimerAdvanceMode::Fire)
                .unwrap();
        assert_eq!(fired.selected_occurrence, 3);
        assert_eq!(fired.skipped_count, 2);
        assert_eq!(fired.next_due_at_ms, Some(at("2026-03-31T00:30:00Z")));
        let autumn = timer(
            ProcessTimerSpec::Daily {
                hour: 2,
                minute: 30,
                total_firings: Some(3),
            },
            "Europe/Warsaw",
            at("2026-10-23T23:00:00Z"),
        );
        assert_eq!(
            due_for_occurrence(&autumn, 2).unwrap(),
            at("2026-10-25T00:30:00Z")
        );
        assert_eq!(
            due_for_occurrence(&autumn, 3).unwrap(),
            at("2026-10-26T01:30:00Z")
        );
        let restore = next_timer_occurrence(
            &autumn,
            at("2026-10-25T01:00:00Z"),
            TimerAdvanceMode::Restore,
        )
        .unwrap();
        assert_eq!(restore.skipped_count, 2);
        assert_eq!(restore.next_occurrence, 3);
        assert_eq!(restore.next_due_at_ms, Some(at("2026-10-26T01:30:00Z")));
        assert!(due_for_occurrence(&autumn, i64::MAX as u64).is_err());
    }

    #[tokio::test]
    async fn timer_start_end_has_real_unique_identity_and_coalesced_history_after_reopen() {
        let fixture = Fixture::new();
        let mut model = super::super::model::starter_model();
        model.timer_timezone = Some("UTC".into());
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::Cycle {
                seconds: 300,
                total_firings: Some(3),
            },
        };
        let version = publish_model(&fixture, &model);
        let summary =
            repository::get_definition(&fixture.db, &fixture.owner, &version.definition_id)
                .unwrap()
                .1
                .unwrap();
        let first_due = summary.due_at_ms.unwrap();
        let at_ms = first_due + 650_000;
        let candidate = repository::due_timers(&fixture.db, at_ms, 32)
            .unwrap()
            .remove(0);
        let snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
        let plan = plan_timer_fire(&snapshot, at_ms).unwrap();
        let id = plan
            .start_instance_id
            .clone()
            .expect("a Start to End plan retains its actual UUID");
        assert!(plan.create_jobs.is_empty() && plan.create_tokens.is_empty());
        let path = fixture.directory.path().join("processes.db");
        let owner = fixture.owner.clone();
        let Fixture {
            directory,
            db,
            router,
            ..
        } = fixture;
        drop(router);
        drop(db);
        let reopened = crate::db::init(&path).unwrap();
        let other_candidate = repository::due_timers(&reopened, at_ms, 32)
            .unwrap()
            .remove(0);
        let other_snapshot = repository::timer_snapshot(&reopened, &other_candidate).unwrap();
        let other_plan = plan_timer_fire(&other_snapshot, at_ms).unwrap();
        let other_id = other_plan.start_instance_id.clone().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let first_barrier = barrier.clone();
            let (pool, first_candidate, actor, first_plan) = (&reopened, &candidate, &owner, &plan);
            let first = scope.spawn(move || {
                first_barrier.wait();
                repository::fire_timer(pool, first_candidate, actor, None, first_plan, at_ms)
                    .unwrap()
            });
            let second = scope.spawn(|| {
                barrier.wait();
                repository::fire_timer(
                    &reopened,
                    &other_candidate,
                    &owner,
                    None,
                    &other_plan,
                    at_ms,
                )
                .unwrap()
            });
            vec![first.join().unwrap(), second.join().unwrap()]
        });
        let committed = results.into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(committed.len(), 1);
        let first = &committed[0].instance;
        assert!(first.instance_id == id || first.instance_id == other_id);
        assert_eq!(first.status, ProcessInstanceStatus::Completed);
        assert!(
            repository::fire_timer(&reopened, &candidate, &owner, None, &plan, at_ms)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            {
                let drained = drain_due(&reopened, at_ms + 10_000);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            0
        );
        let instances =
            repository::list_instances(&reopened, &owner, Some(&version.definition_id), 0, 100)
                .unwrap();
        assert_eq!(instances.1, 1);
        let history = repository::list_events(&reopened, &owner, &first.instance_id, 0, 200)
            .unwrap()
            .0;
        let fired = history
            .iter()
            .find(|event| event.kind == "timer_fired")
            .unwrap();
        assert_eq!(fired.data["occurrence"], 3);
        assert_eq!(fired.data["planned_due_at_ms"], first_due + 600_000);
        assert_eq!(fired.data["fired_at_ms"], at_ms);
        assert_eq!(fired.data["skipped_count"], 2);
        assert_eq!(fired.data["skipped_from_occurrence"], 1);
        assert_eq!(fired.data["skipped_through_occurrence"], 2);
        assert_eq!(
            repository::get_definition(&reopened, &owner, &version.definition_id)
                .unwrap()
                .1
                .unwrap()
                .status,
            ProcessTimerStatus::Fired
        );
        drop(reopened);
        drop(directory);
    }

    #[tokio::test]
    async fn catch_due_survives_restart_and_cancel_races_without_resurrection() {
        let fixture = Fixture::new();
        let at_ms = at("2026-01-01T00:00:00Z");
        let waiting = start_at(
            &fixture,
            &catch_model(ProcessTimerSpec::Duration { seconds: 60 }),
            at_ms,
        );
        let summary = &waiting.timers[0];
        assert_eq!(summary.due_at_ms, Some(at_ms + 60_000));
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
                .unwrap();
        assert_eq!(snapshot.timers[0].anchor_at_ms, at_ms);
        assert_eq!(
            snapshot.timers[0].token_id.as_ref(),
            Some(&snapshot.tokens[0].token_id)
        );
        let owner = fixture.owner.clone();
        let path = fixture.directory.path().join("processes.db");
        let Fixture {
            directory,
            db,
            router,
            ..
        } = fixture;
        drop(router);
        drop(db);
        let reopened = crate::db::init(&path).unwrap();
        assert!(repository::due_timers(&reopened, at_ms + 59_999, 32)
            .unwrap()
            .is_empty());
        let candidate = repository::due_timers(&reopened, at_ms + 60_000, 32)
            .unwrap()
            .remove(0);
        let timer_snapshot = repository::timer_snapshot(&reopened, &candidate).unwrap();
        let plan = plan_timer_fire(&timer_snapshot, at_ms + 60_000).unwrap();
        repository::cancel_instance(
            &reopened,
            &owner,
            &stamp("cancel before timer commit"),
            &waiting.instance_id,
            waiting.revision,
        )
        .unwrap();
        assert!(repository::fire_timer(
            &reopened,
            &candidate,
            &owner,
            Some(waiting.revision),
            &plan,
            at_ms + 60_000
        )
        .unwrap()
        .is_none());
        assert!(!repository::record_timer_failed(
            &reopened,
            &candidate,
            "a late planning failure",
            at_ms + 60_000
        )
        .unwrap());
        assert_eq!(
            {
                let drained = drain_due(&reopened, at_ms + 120_000);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            0
        );
        let cancelled = repository::get_instance(&reopened, &owner, &waiting.instance_id).unwrap();
        assert_eq!(cancelled.timers[0].status, ProcessTimerStatus::Cancelled);
        assert!(
            repository::list_events(&reopened, &owner, &waiting.instance_id, 0, 200)
                .unwrap()
                .0
                .iter()
                .all(|event| event.kind != "timer_fired")
        );
        drop(reopened);
        drop(directory);
    }

    #[tokio::test]
    async fn past_date_catch_fires_once_and_duration_catch_continues_parallel_activation() {
        let fixture = Fixture::new();
        let at_ms = at("2026-01-01T00:00:00Z");
        let past = start_at(
            &fixture,
            &catch_model(ProcessTimerSpec::Date {
                at: "2025-12-31T23:59:59Z".into(),
            }),
            at_ms,
        );
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, at_ms);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            1
        );
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, at_ms);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            0
        );
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &past.instance_id)
                .unwrap()
                .status,
            ProcessInstanceStatus::Completed
        );
        let mut model = catch_model(ProcessTimerSpec::Duration { seconds: 60 });
        model.nodes.extend([
            ProcessNode {
                id: "Split".into(),
                name: "Parallel wait".into(),
                kind: ProcessNodeKind::ParallelGateway,
            },
            ProcessNode {
                id: "Review".into(),
                name: "Independent review".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
            },
            ProcessNode {
                id: "Join".into(),
                name: "Wait for both branches".into(),
                kind: ProcessNodeKind::ParallelGateway,
            },
        ]);
        model.sequence_flows = vec![
            edge("ToSplit", "Start_1", "Split"),
            edge("WaitBranch", "Split", "Wait"),
            edge("ReviewBranch", "Split", "Review"),
            edge("WaitJoin", "Wait", "Join"),
            edge("ReviewJoin", "Review", "Join"),
            edge("JoinEnd", "Join", "End_1"),
        ];
        let waiting = start_at(&fixture, &model, at_ms);
        let candidate = repository::due_timers(&fixture.db, at_ms + 60_000, 32)
            .unwrap()
            .remove(0);
        let snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
        let plan = plan_timer_fire(&snapshot, at_ms + 60_000).unwrap();
        let partial = repository::fire_timer(
            &fixture.db,
            &candidate,
            &fixture.owner,
            Some(waiting.revision),
            &plan,
            at_ms + 60_000,
        )
        .unwrap()
        .unwrap();
        assert!(partial.cancelled_claims.is_empty());
        let partial = partial.instance;
        assert_eq!(partial.status, ProcessInstanceStatus::Waiting);
        let owner = fixture.owner.clone();
        let path = fixture.directory.path().join("processes.db");
        let Fixture {
            directory,
            db,
            router,
            ..
        } = fixture;
        drop(router);
        drop(db);
        let reopened = crate::db::init(&path).unwrap();
        let snapshot =
            repository::runtime_snapshot(&reopened, &owner, &waiting.instance_id).unwrap();
        assert_eq!(snapshot.receipts.len(), 1);
        assert_eq!(snapshot.receipts[0].branch_edge_id, "WaitBranch");
        let task = snapshot
            .user_tasks
            .iter()
            .find(|task| task.status == ProcessUserTaskStatus::Open)
            .unwrap();
        let plan = runtime::plan_user_completion(
            &snapshot,
            &task.user_task_id,
            &json!({}),
            None,
            at_ms + 61_000,
        )
        .unwrap();
        let completed = repository::complete_user_task(
            &reopened,
            &owner,
            &stamp("review after due"),
            &waiting.instance_id,
            &task.user_task_id,
            partial.revision,
            &json!({}),
            None,
            &plan,
            at_ms + 61_000,
        )
        .unwrap();
        assert_eq!(completed.status, ProcessInstanceStatus::Completed);
        let events = repository::list_events(&reopened, &owner, &waiting.instance_id, 0, 200)
            .unwrap()
            .0;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "parallel_joined")
                .count(),
            1
        );
        assert!(repository::fire_timer(
            &reopened,
            &candidate,
            &owner,
            Some(waiting.revision),
            &plan,
            at_ms + 61_000
        )
        .unwrap()
        .is_none());
        assert_eq!(
            {
                let drained = drain_due(&reopened, at_ms + 120_000);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            0
        );
        drop(reopened);
        drop(directory);
    }

    #[tokio::test]
    async fn current_flow_and_account_revocation_block_fire_with_bounded_reason_changed_retry() {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("authorized", None));
        let mut model = service_model(
            &flow_id,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        );
        model.timer_timezone = Some("UTC".into());
        model.nodes.insert(
            1,
            ProcessNode {
                id: "Wait".into(),
                name: "Wait before authorized execution".into(),
                kind: ProcessNodeKind::TimerCatch {
                    timer: ProcessTimerSpec::Duration { seconds: 60 },
                },
            },
        );
        model.sequence_flows = vec![
            edge("ToWait", "Start_1", "Wait"),
            edge("ToService", "Wait", "Service"),
            edge("ToEnd", "Service", "End_1"),
        ];
        let at_ms = at("2026-01-01T00:00:00Z");
        let waiting = start_at(&fixture, &model, at_ms);
        let due = at_ms + 60_000;
        crate::db::repository::resource_permissions::set(
            &fixture.db,
            "flow",
            &flow_id,
            "user",
            &fixture.owner.user_id,
            "deny",
        )
        .unwrap();
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, due);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            0
        );
        assert!(repository::due_timers(&fixture.db, due + 59_999, 32)
            .unwrap()
            .is_empty());
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, due + 60_000);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            0
        );
        let history =
            repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 200)
                .unwrap()
                .0;
        assert_eq!(
            history
                .iter()
                .filter(|event| event.kind == "timer_blocked")
                .count(),
            1
        );
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id)
                .unwrap()
                .timers[0]
                .status,
            ProcessTimerStatus::Blocked
        );
        assert!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                .unwrap()
                .is_empty()
        );
        crate::db::repository::resource_permissions::set(
            &fixture.db,
            "flow",
            &flow_id,
            "user",
            &fixture.owner.user_id,
            "allow",
        )
        .unwrap();
        let candidate = repository::due_timers(&fixture.db, due + 120_000, 32)
            .unwrap()
            .remove(0);
        let snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
        let plan = plan_timer_fire(&snapshot, due + 120_000).unwrap();
        crate::db::repository::update_user_account(
            &fixture.db,
            &fixture.owner.user_id,
            "Process owner",
            "owner@example.test",
            false,
        )
        .unwrap();
        let error = repository::fire_timer(
            &fixture.db,
            &candidate,
            &fixture.owner,
            Some(waiting.revision),
            &plan,
            due + 120_000,
        )
        .unwrap_err();
        assert!(error
            .downcast_ref::<repository::ProcessAuthorityDenied>()
            .is_some());
        assert!(repository::record_timer_blocked(
            &fixture.db,
            &candidate,
            &error.to_string(),
            due + 120_000
        )
        .unwrap());
        assert!(
            repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id).is_err()
        );
        crate::db::repository::update_user_account(
            &fixture.db,
            &fixture.owner.user_id,
            "Process owner",
            "owner@example.test",
            true,
        )
        .unwrap();
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, due + 180_000);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            1
        );
        let current =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
                .unwrap();
        assert_eq!(current.jobs.len(), 1);
        assert_eq!(current.jobs[0].status, "queued");
        let history =
            repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 200)
                .unwrap()
                .0;
        assert_eq!(
            history
                .iter()
                .filter(|event| event.kind == "timer_blocked")
                .count(),
            2
        );
        let fired = history
            .iter()
            .find(|event| event.kind == "timer_fired")
            .unwrap();
        assert_eq!(fired.data["planned_due_at_ms"], due);
        assert_eq!(fired.data["fired_at_ms"], due + 180_000);
        assert_eq!(fired.data["skipped_count"], 0);
    }

    #[tokio::test]
    async fn horizon_error_is_terminal_and_parallel_completion_preserves_catch_incident() {
        let fixture = Fixture::new();
        let mut repeated = super::super::model::starter_model();
        repeated.timer_timezone = Some("UTC".into());
        repeated.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::Cycle {
                seconds: 300,
                total_firings: None,
            },
        };
        let version = publish_model(&fixture, &repeated);
        let before =
            repository::get_definition(&fixture.db, &fixture.owner, &version.definition_id)
                .unwrap()
                .1
                .unwrap();
        let horizon = DateTime::<Utc>::MAX_UTC.timestamp_millis();
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, horizon);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            0
        );
        let failed =
            repository::get_definition(&fixture.db, &fixture.owner, &version.definition_id)
                .unwrap()
                .1
                .unwrap();
        assert_eq!(failed.status, ProcessTimerStatus::Error);
        assert_eq!(failed.due_at_ms, before.due_at_ms);
        assert_eq!(failed.occurrence, 1);
        assert!(failed.last_reason.as_deref().unwrap().contains("horizon"));
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, horizon);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            0
        );
        assert_eq!(
            repository::get_definition(&fixture.db, &fixture.owner, &version.definition_id)
                .unwrap()
                .1
                .unwrap(),
            failed
        );
        assert_eq!(
            repository::list_instances(
                &fixture.db,
                &fixture.owner,
                Some(&version.definition_id),
                0,
                100
            )
            .unwrap()
            .1,
            0
        );

        let mut model = catch_model(ProcessTimerSpec::Duration { seconds: 60 });
        model.nodes.extend([
            ProcessNode {
                id: "Split".into(),
                name: "Parallel".into(),
                kind: ProcessNodeKind::ParallelGateway,
            },
            ProcessNode {
                id: "Review".into(),
                name: "Independent review".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
            },
            ProcessNode {
                id: "Join".into(),
                name: "Join both".into(),
                kind: ProcessNodeKind::ParallelGateway,
            },
        ]);
        model.sequence_flows = vec![
            edge("ToSplit", "Start_1", "Split"),
            edge("WaitBranch", "Split", "Wait"),
            edge("ReviewBranch", "Split", "Review"),
            edge("WaitJoin", "Wait", "Join"),
            edge("ReviewJoin", "Review", "Join"),
            edge("JoinEnd", "Join", "End_1"),
        ];
        let at_ms = at("2026-01-01T00:00:00Z");
        let waiting = start_at(&fixture, &model, at_ms);
        let candidate = repository::due_timers(&fixture.db, at_ms + 60_000, 32)
            .unwrap()
            .remove(0);
        let reason = resolve_timer_due(
            &ProcessTimerSpec::Duration { seconds: 1 },
            "UTC",
            ProcessTimerKind::Catch,
            horizon,
        )
        .unwrap_err()
        .to_string();
        assert!(
            repository::record_timer_failed(&fixture.db, &candidate, &reason, at_ms + 60_000)
                .unwrap()
        );
        assert!(
            !repository::record_timer_failed(&fixture.db, &candidate, &reason, at_ms + 60_000)
                .unwrap()
        );
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
                .unwrap();
        assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Incident);
        assert_eq!(
            snapshot.instance.timers[0].status,
            ProcessTimerStatus::Error
        );
        assert!(snapshot.tokens.iter().any(|token| Some(&token.token_id)
            == snapshot.timers[0].token_id.as_ref()
            && token.status == "waiting"));
        let task = snapshot
            .user_tasks
            .iter()
            .find(|task| task.status == ProcessUserTaskStatus::Open)
            .unwrap();
        let plan = runtime::plan_user_completion(
            &snapshot,
            &task.user_task_id,
            &json!({}),
            None,
            at_ms + 61_000,
        )
        .unwrap();
        let still_incident = repository::complete_user_task(
            &fixture.db,
            &fixture.owner,
            &stamp("finish unaffected branch"),
            &waiting.instance_id,
            &task.user_task_id,
            snapshot.instance.revision,
            &json!({}),
            None,
            &plan,
            at_ms + 61_000,
        )
        .unwrap();
        assert_eq!(still_incident.status, ProcessInstanceStatus::Incident);
        assert_eq!(still_incident.incidents.len(), 1);
        assert_eq!(still_incident.incidents[0].code, "TIMER_ERROR");
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, at_ms + 120_000);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            0
        );
        let events =
            repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 200)
                .unwrap()
                .0;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "timer_error")
                .count(),
            1
        );
        assert!(events
            .iter()
            .all(|event| event.kind != "instance_completed"));
    }

    #[tokio::test]
    async fn timer_service_continuation_uses_pinned_graph_and_same_time_catch_anchor() {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("published", None));
        let mut model = service_model(
            &flow_id,
            ActivityVerification::Condition {
                expression: "outputs.variables.marker == 'published'".into(),
            },
        );
        model.timer_timezone = Some("UTC".into());
        model.nodes.extend([
            ProcessNode {
                id: "Before".into(),
                name: "Wait before service".into(),
                kind: ProcessNodeKind::TimerCatch {
                    timer: ProcessTimerSpec::Duration { seconds: 1 },
                },
            },
            ProcessNode {
                id: "After".into(),
                name: "Wait after service result".into(),
                kind: ProcessNodeKind::TimerCatch {
                    timer: ProcessTimerSpec::Duration { seconds: 1 },
                },
            },
        ]);
        model.sequence_flows = vec![
            edge("ToBefore", "Start_1", "Before"),
            edge("ToService", "Before", "Service"),
            edge("ToAfter", "Service", "After"),
            edge("ToEnd", "After", "End_1"),
        ];
        let at_ms = Utc::now().timestamp_millis() - 2_000;
        let waiting = start_at(&fixture, &model, at_ms);
        update_flow(
            &fixture.db,
            &fixture.owner,
            &flow_id,
            &graph("edited", None),
        );
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, Utc::now().timestamp_millis());
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            1
        );
        let claim =
            repository::claim_job(&fixture.db, "timer-service", Utc::now().timestamp_millis())
                .unwrap()
                .unwrap();
        super::super::jobs::execute_claimed(
            &fixture.db,
            fixture.dispatcher(),
            "timer-service",
            claim,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
                .unwrap();
        assert_eq!(snapshot.instance.variables["answer"], "published");
        let after = snapshot
            .timers
            .iter()
            .find(|timer| timer.node_id == "After")
            .unwrap();
        let history =
            repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 200)
                .unwrap()
                .0;
        let result = history
            .iter()
            .find(|event| event.kind == "service_result")
            .unwrap();
        assert_eq!(after.anchor_at_ms, result.at_ms);
        assert_eq!(after.due_at_ms, Some(result.at_ms + 1_000));
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, after.due_at_ms.unwrap());
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            1
        );
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id)
                .unwrap()
                .status,
            ProcessInstanceStatus::Completed
        );
        let executions =
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                .unwrap();
        assert_eq!(executions.len(), 1);
        let history =
            repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 200)
                .unwrap()
                .0;
        assert_eq!(
            history
                .iter()
                .filter(|event| event.kind == "timer_fired")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn existing_worker_drains_overdue_timer_and_shutdown_preserves_future_catch() {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("worker", None));
        let mut model = service_model(
            &flow_id,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        );
        model.timer_timezone = Some("UTC".into());
        model.nodes.insert(
            1,
            ProcessNode {
                id: "Wait".into(),
                name: "Wait before service".into(),
                kind: ProcessNodeKind::TimerCatch {
                    timer: ProcessTimerSpec::Duration { seconds: 1 },
                },
            },
        );
        model.sequence_flows = vec![
            edge("ToWait", "Start_1", "Wait"),
            edge("ToService", "Wait", "Service"),
            edge("ToEnd", "Service", "End_1"),
        ];
        let waiting = start_at(&fixture, &model, Utc::now().timestamp_millis() - 2_000);
        let future = start_at(
            &fixture,
            &catch_model(ProcessTimerSpec::Duration { seconds: 60 }),
            Utc::now().timestamp_millis(),
        );
        let worker = runtime::start(&fixture.db, fixture.dispatcher()).unwrap();
        let same = runtime::start(&fixture.db, fixture.dispatcher()).unwrap();
        assert!(std::sync::Arc::ptr_eq(&worker, &same));
        runtime::wake(fixture.dispatcher());
        wait_for_status(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            ProcessInstanceStatus::Completed,
        )
        .await;
        runtime::stop(fixture.dispatcher()).await.unwrap();
        let still_waiting =
            repository::get_instance(&fixture.db, &fixture.owner, &future.instance_id).unwrap();
        assert_eq!(still_waiting.status, ProcessInstanceStatus::Waiting);
        assert_eq!(still_waiting.timers[0].status, ProcessTimerStatus::Pending);
        assert_eq!(
            still_waiting.timers[0].due_at_ms,
            future.timers[0].due_at_ms
        );
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, future.timers[0].due_at_ms.unwrap());
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            1
        );
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &future.instance_id)
                .unwrap()
                .status,
            ProcessInstanceStatus::Completed
        );
    }

    #[tokio::test]
    async fn one_drain_handles_at_most_32_due_candidates() {
        let fixture = Fixture::new();
        let model = catch_model(ProcessTimerSpec::Duration { seconds: 1 });
        let version = publish_model(&fixture, &model);
        let at_ms = at("2026-01-01T00:00:00Z");
        for _ in 0..33 {
            let id = Uuid::new_v4().to_string();
            let plan = runtime::plan_start(
                &model,
                &id,
                &fixture.owner,
                &version.definition_id,
                version.version,
                json!({}),
                StartCause::Manual,
                at_ms,
            )
            .unwrap();
            repository::start_instance(
                &fixture.db,
                &fixture.owner,
                &stamp("bounded timer start"),
                &id,
                &version.definition_id,
                version.version,
                &json!({}),
                &plan,
                at_ms,
            )
            .unwrap();
        }
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, at_ms + 1_000);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            32
        );
        assert_eq!(
            repository::due_timers(&fixture.db, at_ms + 1_000, 32)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, at_ms + 1_000);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            1
        );
        assert_eq!(
            {
                let drained = drain_due(&fixture.db, at_ms + 1_000);
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            0
        );
        assert_eq!(
            repository::list_instances(
                &fixture.db,
                &fixture.owner,
                Some(&version.definition_id),
                0,
                100
            )
            .unwrap()
            .1,
            33
        );
    }

    fn current_boundary(
        fixture: &Fixture,
        instance_id: &str,
        node_id: &str,
        at_ms: i64,
    ) -> (repository::DueTimer, TimerSnapshot, repository::RuntimePlan) {
        let candidate = repository::due_timers(&fixture.db, at_ms, 32)
            .unwrap()
            .into_iter()
            .find(|candidate| {
                candidate.instance_id.as_deref() == Some(instance_id)
                    && repository::timer_snapshot(&fixture.db, candidate).is_ok_and(|snapshot| {
                        match snapshot {
                            TimerSnapshot::Boundary { timer, .. } => timer.node_id == node_id,
                            _ => false,
                        }
                    })
            })
            .expect("actual due boundary");
        let snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
        let plan = plan_timer_fire(&snapshot, at_ms).unwrap();
        (candidate, snapshot, plan)
    }

    fn complete_work(
        fixture: &Fixture,
        instance_id: &str,
        task_id: &str,
        at_ms: i64,
    ) -> ProcessInstance {
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, instance_id).unwrap();
        let task = snapshot
            .user_tasks
            .iter()
            .find(|task| task.user_task_id == task_id)
            .unwrap();
        let actor = if task.assignee_user_id == fixture.owner.user_id {
            &fixture.owner
        } else {
            &fixture.participant
        };
        let outputs = json!({"answer":"accepted"});
        let plan =
            runtime::plan_user_completion(&snapshot, task_id, &outputs, None, at_ms).unwrap();
        repository::complete_user_task(
            &fixture.db,
            actor,
            &stamp("complete exact work"),
            instance_id,
            task_id,
            snapshot.instance.revision,
            &outputs,
            None,
            &plan,
            at_ms,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn boundary_completion_before_fire_disarms_exact_waiting_uuid_and_all_siblings() {
        let fixture = Fixture::new();
        let model = with_boundaries(
            user_model(Some(&fixture.participant.user_id)),
            "Work",
            &[("Limit_A", true, 1), ("Limit_B", false, 1)],
        );
        let started = start_at(&fixture, &model, 1_000);
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        let token = &snapshot.tokens[0];
        assert_eq!(token.status, "waiting");
        assert_eq!(
            snapshot.user_tasks[0].token_id.as_deref(),
            Some(token.token_id.as_str())
        );
        assert_eq!(snapshot.timers.len(), 2);
        assert!(snapshot
            .timers
            .iter()
            .all(|timer| timer.token_id.as_deref() == Some(token.token_id.as_str())));
        let (candidate, _, plan) =
            current_boundary(&fixture, &started.instance_id, "Limit_A", 2_000);
        let completed = complete_work(
            &fixture,
            &started.instance_id,
            &snapshot.user_tasks[0].user_task_id,
            2_000,
        );
        assert_eq!(completed.status, ProcessInstanceStatus::Completed);
        assert!(completed
            .timers
            .iter()
            .all(|timer| timer.status == ProcessTimerStatus::Cancelled
                && timer.last_reason.as_deref() == Some("activity_completed")));
        assert!(repository::fire_timer(
            &fixture.db,
            &candidate,
            &fixture.owner,
            Some(started.revision),
            &plan,
            2_000
        )
        .unwrap()
        .is_none());
        let events =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "timer_cancelled")
                .count(),
            2
        );
        assert!(events.iter().all(|event| event.kind != "timer_fired"));
    }

    #[tokio::test]
    async fn boundary_interrupting_siblings_compete_by_actual_commit_and_late_completion_is_rejected(
    ) {
        let fixture = Fixture::new();
        let mut model = with_boundaries(
            user_model(None),
            "Work",
            &[("Limit_A", true, 1), ("Limit_B", true, 1)],
        );
        let ProcessNodeKind::BoundaryTimer { timer, .. } = &mut model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Limit_B")
            .unwrap()
            .kind
        else {
            panic!("the fixture boundary node is missing");
        };
        *timer = ProcessTimerSpec::Date {
            at: "1970-01-01T00:00:02.000Z".into(),
        };
        let started = start_at(&fixture, &model, 1_000);
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        let completion = runtime::plan_user_completion(
            &snapshot,
            &snapshot.user_tasks[0].user_task_id,
            &json!({"answer":"late"}),
            None,
            2_000,
        )
        .unwrap();
        let first = current_boundary(&fixture, &started.instance_id, "Limit_A", 2_000);
        let second = current_boundary(&fixture, &started.instance_id, "Limit_B", 2_000);
        let barrier = std::sync::Barrier::new(2);
        let results = std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                barrier.wait();
                repository::fire_timer(
                    &fixture.db,
                    &first.0,
                    &fixture.owner,
                    Some(started.revision),
                    &first.2,
                    2_000,
                )
                .unwrap()
            });
            let b = scope.spawn(|| {
                barrier.wait();
                repository::fire_timer(
                    &fixture.db,
                    &second.0,
                    &fixture.owner,
                    Some(started.revision),
                    &second.2,
                    2_000,
                )
                .unwrap()
            });
            vec![a.join().unwrap(), b.join().unwrap()]
        });
        assert_eq!(results.into_iter().flatten().count(), 1);
        let current =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(current.instance.status, ProcessInstanceStatus::Completed);
        assert_eq!(
            current.user_tasks[0].status,
            ProcessUserTaskStatus::Cancelled
        );
        assert_eq!(
            current
                .timers
                .iter()
                .filter(|timer| timer.status == ProcessTimerStatus::Fired)
                .count(),
            1
        );
        assert_eq!(
            current
                .timers
                .iter()
                .filter(|timer| timer.status == ProcessTimerStatus::Cancelled
                    && timer.last_reason.as_deref() == Some("sibling_interrupted"))
                .count(),
            1
        );
        assert!(repository::complete_user_task(
            &fixture.db,
            &fixture.owner,
            &stamp("late completion"),
            &started.instance_id,
            &snapshot.user_tasks[0].user_task_id,
            started.revision,
            &json!({"answer":"late"}),
            None,
            &completion,
            2_000
        )
        .is_err());
        let events =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "timer_fired")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn noninterrupting_branch_survives_later_interruption_restart_and_duplicate_delivery() {
        let fixture = Fixture::new();
        let mut model = with_boundaries(
            user_model(None),
            "Work",
            &[("Reminder", false, 1), ("Limit", true, 2)],
        );
        model.nodes.push(ProcessNode {
            id: "FollowUp".into(),
            name: "Reminder follow-up".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            },
        });
        model
            .sequence_flows
            .iter_mut()
            .find(|edge| edge.source_id == "Reminder")
            .unwrap()
            .target_id = "FollowUp".into();
        model
            .sequence_flows
            .push(edge("FollowUpDone", "FollowUp", "End_1"));
        let started = start_at(&fixture, &model, 1_000);
        let (first, _, plan) = current_boundary(&fixture, &started.instance_id, "Reminder", 2_000);
        let fired = repository::fire_timer(
            &fixture.db,
            &first,
            &fixture.owner,
            Some(started.revision),
            &plan,
            2_000,
        )
        .unwrap()
        .unwrap();
        assert_eq!(fired.instance.status, ProcessInstanceStatus::Waiting);
        let directory = fixture.directory.path().to_owned();
        let actor = fixture.owner.clone();
        let participant = fixture.participant.clone();
        let Fixture {
            directory: keep_directory,
            db,
            router,
            ..
        } = fixture;
        drop(router);
        drop(db);
        let db = crate::db::init(&directory.join("processes.db")).unwrap();
        let router = std::sync::Arc::new(
            crate::routing::Router::new(crate::config::RouterConfig::default(), Some(db.clone()))
                .unwrap(),
        );
        let fixture = Fixture {
            directory: keep_directory,
            db,
            router,
            owner: actor,
            participant,
        };
        let before =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(
            before
                .user_tasks
                .iter()
                .filter(|task| task.status == ProcessUserTaskStatus::Open)
                .count(),
            2
        );
        let side = before
            .user_tasks
            .iter()
            .find(|task| task.node_id == "FollowUp")
            .unwrap()
            .clone();
        let (second, _, plan2) = current_boundary(&fixture, &started.instance_id, "Limit", 3_000);
        repository::fire_timer(
            &fixture.db,
            &second,
            &fixture.owner,
            Some(before.instance.revision),
            &plan2,
            3_000,
        )
        .unwrap()
        .unwrap();
        let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
            .unwrap();
        assert_eq!(
            after
                .user_tasks
                .iter()
                .find(|task| task.node_id == "Work")
                .unwrap()
                .status,
            ProcessUserTaskStatus::Cancelled
        );
        assert_eq!(
            after
                .user_tasks
                .iter()
                .find(|task| task.user_task_id == side.user_task_id)
                .unwrap()
                .status,
            ProcessUserTaskStatus::Open
        );
        assert!(after
            .tokens
            .iter()
            .any(|token| side.token_id.as_deref() == Some(token.token_id.as_str())));
        assert!(repository::fire_timer(
            &fixture.db,
            &first,
            &fixture.owner,
            Some(started.revision),
            &plan,
            3_000
        )
        .unwrap()
        .is_none());
        assert!(repository::fire_timer(
            &fixture.db,
            &second,
            &fixture.owner,
            Some(before.instance.revision),
            &plan2,
            3_000
        )
        .unwrap()
        .is_none());
        assert_eq!(
            complete_work(&fixture, &started.instance_id, &side.user_task_id, 3_001).status,
            ProcessInstanceStatus::Completed
        );
    }

    #[tokio::test]
    async fn boundary_human_verification_remains_armed_and_interruption_preserves_accepted_service_result(
    ) {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("pinned result", None));
        let model = with_boundaries(
            service_model(&flow_id, ActivityVerification::Human),
            "Service",
            &[("Limit", true, 1)],
        );
        let at_ms = Utc::now().timestamp_millis();
        let started = start_at(&fixture, &model, at_ms);
        let claim = repository::claim_job(&fixture.db, "human-service", at_ms)
            .unwrap()
            .unwrap();
        super::super::jobs::execute_claimed(
            &fixture.db,
            fixture.dispatcher(),
            "human-service",
            claim.clone(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        let before =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(before.jobs[0].status, "completed");
        let accepted_result = before.jobs[0].result.clone().unwrap();
        assert_eq!(before.timers[0].status, ProcessTimerStatus::Pending);
        assert_eq!(
            before.user_tasks[0].kind,
            tentaflow_protocol::processes::ProcessUserTaskKind::Verification
        );
        assert_eq!(
            before.user_tasks[0].token_id.as_deref(),
            Some(claim.job.token_id.as_str())
        );
        let (candidate, _, plan) =
            current_boundary(&fixture, &started.instance_id, "Limit", at_ms + 1_000);
        let outcome = repository::fire_timer(
            &fixture.db,
            &candidate,
            &fixture.owner,
            Some(before.instance.revision),
            &plan,
            at_ms + 1_000,
        )
        .unwrap()
        .unwrap();
        assert!(outcome.cancelled_claims.is_empty());
        let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
            .unwrap();
        assert_eq!(after.jobs[0].status, "completed");
        assert_eq!(after.jobs[0].result, Some(accepted_result.clone()));
        assert_eq!(after.jobs[0].fence, before.jobs[0].fence);
        assert_eq!(after.user_tasks[0].status, ProcessUserTaskStatus::Cancelled);
        assert_eq!(after.instance.status, ProcessInstanceStatus::Completed);
        assert!(!after.instance.can_retry);
        assert!(repository::retry_job(
            &fixture.db,
            &fixture.owner,
            &stamp("retry cancelled verification"),
            &started.instance_id,
            &claim.job.job_id,
            after.instance.revision
        )
        .is_err());
        let replay = repository::accept_job_result(
            &fixture.db,
            &fixture.owner,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "human-service",
            &accepted_result,
            after.instance.revision,
            &repository::RuntimePlan::initial(json!({})),
            at_ms + 1_000,
        )
        .unwrap();
        assert_eq!(replay, after.instance);
    }

    #[tokio::test]
    async fn boundary_error_resolves_on_exact_activity_completion_without_erasing_unrelated_incident(
    ) {
        for unrelated in [false, true] {
            let fixture = Fixture::new();
            let mut model = with_boundaries(user_model(None), "Work", &[("Broken", true, 2)]);
            if unrelated {
                model = with_boundaries(model, "Work", &[("Reminder", false, 1)]);
                model.nodes.push(ProcessNode {
                    id: "Choice".into(),
                    name: "Unmatched side choice".into(),
                    kind: ProcessNodeKind::ExclusiveGateway {
                        default_flow_id: None,
                    },
                });
                model
                    .sequence_flows
                    .iter_mut()
                    .find(|edge| edge.source_id == "Reminder")
                    .unwrap()
                    .target_id = "Choice".into();
                for id in ["Choice_A", "Choice_B"] {
                    let mut branch = edge(id, "Choice", "End_1");
                    branch.condition = Some("false".into());
                    model.sequence_flows.push(branch);
                }
            }
            let started = start_at(&fixture, &model, 1_000);
            if unrelated {
                let drained = drain_due(&fixture.db, 2_000);
                drained.completion.unwrap();
                assert_eq!(drained.fired, 1);
            }
            let timer =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap()
                    .timers
                    .into_iter()
                    .find(|timer| timer.node_id == "Broken")
                    .unwrap();
            fixture.db.write().unwrap().execute_batch(&format!("CREATE TRIGGER reject_boundary_fire BEFORE UPDATE ON bpmn_timers WHEN OLD.timer_id='{}' AND NEW.status='fired' BEGIN SELECT RAISE(ABORT,'controlled boundary storage failure'); END;",timer.timer_id)).unwrap();
            let drained = drain_due(&fixture.db, 3_000);
            drained.completion.unwrap();
            assert_eq!(drained.fired, 0);
            fixture
                .db
                .write()
                .unwrap()
                .execute_batch("DROP TRIGGER reject_boundary_fire")
                .unwrap();
            let failed =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            assert_eq!(
                failed
                    .timers
                    .iter()
                    .find(|actual| actual.timer_id == timer.timer_id)
                    .unwrap()
                    .status,
                ProcessTimerStatus::Error
            );
            assert_eq!(failed.boundary_incidents.len(), 1);
            assert_eq!(
                failed
                    .user_tasks
                    .iter()
                    .find(|task| task.node_id == "Work")
                    .unwrap()
                    .status,
                ProcessUserTaskStatus::Open
            );
            let linked = failed.boundary_incidents[0].incident_id.clone();
            let other = failed
                .instance
                .incidents
                .iter()
                .find(|incident| incident.code == "NO_MATCHING_FLOW")
                .map(|incident| incident.incident_id.clone());
            assert_eq!(other.is_some(), unrelated);
            let task = failed
                .user_tasks
                .iter()
                .find(|task| task.node_id == "Work")
                .unwrap();
            let finished = complete_work(&fixture, &started.instance_id, &task.user_task_id, 3_001);
            assert!(finished
                .incidents
                .iter()
                .all(|incident| incident.incident_id != linked));
            assert_eq!(
                finished.status,
                if unrelated {
                    ProcessInstanceStatus::Incident
                } else {
                    ProcessInstanceStatus::Completed
                }
            );
            assert_eq!(
                finished
                    .incidents
                    .iter()
                    .map(|incident| incident.incident_id.clone())
                    .collect::<Vec<_>>(),
                other.into_iter().collect::<Vec<_>>()
            );
            assert_eq!(
                finished
                    .timers
                    .iter()
                    .find(|actual| actual.timer_id == timer.timer_id)
                    .unwrap()
                    .status,
                ProcessTimerStatus::Error
            );
            assert!(repository::due_timers(&fixture.db, 4_000, 32)
                .unwrap()
                .is_empty());
            let history =
                repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                    .unwrap()
                    .0;
            let error = history
                .iter()
                .find(|event| event.kind == "timer_error")
                .unwrap();
            assert_eq!(error.data["incident_id"], linked);
            assert_eq!(
                error.data["attached_token_id"],
                task.token_id.clone().unwrap()
            );
            assert!(error.data["reason"]
                .as_str()
                .unwrap()
                .contains("controlled boundary storage failure"));
        }
    }

    #[tokio::test]
    async fn boundary_whole_cancel_resolves_linked_error_and_records_all_remaining_siblings() {
        let fixture = Fixture::new();
        let model = with_boundaries(
            user_model(None),
            "Work",
            &[("Broken", true, 1), ("Pending", false, 2)],
        );
        let started = start_at(&fixture, &model, 1_000);
        let (candidate, _, _) = current_boundary(&fixture, &started.instance_id, "Broken", 2_000);
        fixture.db.write().unwrap().execute_batch(&format!("CREATE TRIGGER fail_cancel_boundary BEFORE UPDATE ON bpmn_timers WHEN OLD.timer_id='{}' AND NEW.status='fired' BEGIN SELECT RAISE(ABORT,'controlled failed boundary commit'); END;",candidate.timer_id)).unwrap();
        let failed = drain_due(&fixture.db, 2_000);
        failed.completion.unwrap();
        assert_eq!(failed.fired, 0);
        fixture
            .db
            .write()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_cancel_boundary")
            .unwrap();
        let before =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        let cancelled = repository::cancel_instance(
            &fixture.db,
            &fixture.owner,
            &stamp("cancel boundary error activation"),
            &started.instance_id,
            before.revision,
        )
        .unwrap();
        assert_eq!(cancelled.status, ProcessInstanceStatus::Cancelled);
        assert!(cancelled.incidents.is_empty());
        assert!(repository::due_timers(&fixture.db, 10_000, 32)
            .unwrap()
            .is_empty());
        let events =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        let event = events
            .iter()
            .find(|event| event.kind == "timer_cancelled")
            .unwrap();
        assert_eq!(event.data["kind"], "Boundary");
        assert_eq!(event.data["attached_to_id"], "Work");
        assert_eq!(event.data["reason"], "instance_cancelled");
    }

    #[tokio::test]
    async fn boundary_fire_rechecks_current_authority_after_snapshot_without_cancelling_work() {
        let fixture = Fixture::new();
        let model = with_boundaries(
            user_model(Some(&fixture.participant.user_id)),
            "Work",
            &[("Limit", true, 1)],
        );
        let started = start_at(&fixture, &model, 1_000);
        let (candidate, _, plan) = current_boundary(&fixture, &started.instance_id, "Limit", 2_000);
        crate::db::repository::update_user_account(
            &fixture.db,
            &fixture.participant.user_id,
            "Participant",
            "participant@example.test",
            false,
        )
        .unwrap();
        let error = repository::fire_timer(
            &fixture.db,
            &candidate,
            &fixture.owner,
            Some(started.revision),
            &plan,
            2_000,
        )
        .unwrap_err();
        assert!(error
            .downcast_ref::<repository::ProcessAuthorityDenied>()
            .is_some());
        assert!(repository::record_timer_blocked(
            &fixture.db,
            &candidate,
            &error.to_string(),
            2_000
        )
        .unwrap());
        let blocked =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(blocked.timers[0].status, ProcessTimerStatus::Blocked);
        assert_eq!(blocked.user_tasks[0].status, ProcessUserTaskStatus::Open);
        assert_eq!(blocked.instance.revision, started.revision);
        assert!(repository::due_timers(&fixture.db, 61_999, 32)
            .unwrap()
            .is_empty());
        crate::db::repository::update_user_account(
            &fixture.db,
            &fixture.participant.user_id,
            "Participant",
            "participant@example.test",
            true,
        )
        .unwrap();
        let resumed = drain_due(&fixture.db, 62_000);
        resumed.completion.unwrap();
        assert_eq!(resumed.fired, 1);
        let after =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(after.status, ProcessInstanceStatus::Completed);
        assert_eq!(after.timers[0].status, ProcessTimerStatus::Fired);
    }

    #[tokio::test]
    async fn boundary_after_real_parallel_join_enters_its_own_closed_parallel_region() {
        let fixture = Fixture::new();
        let mut model = with_boundaries(user_model(None), "Work", &[("Limit", true, 1)]);
        for id in ["Split", "Join", "SideSplit", "SideJoin"] {
            model.nodes.push(ProcessNode {
                id: id.into(),
                name: id.into(),
                kind: ProcessNodeKind::ParallelGateway,
            });
        }
        for id in ["Main_A", "Main_B", "Side_A", "Side_B"] {
            model.nodes.push(ProcessNode {
                id: id.into(),
                name: id.into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
            });
        }
        model
            .sequence_flows
            .iter_mut()
            .find(|edge| edge.source_id == "Start_1")
            .unwrap()
            .target_id = "Split".into();
        model
            .sequence_flows
            .iter_mut()
            .find(|edge| edge.source_id == "Limit")
            .unwrap()
            .target_id = "SideSplit".into();
        for (id, source, target) in [
            ("M_A", "Split", "Main_A"),
            ("M_B", "Split", "Main_B"),
            ("M_C", "Main_A", "Join"),
            ("M_D", "Main_B", "Join"),
            ("M_E", "Join", "Work"),
            ("S_A", "SideSplit", "Side_A"),
            ("S_B", "SideSplit", "Side_B"),
            ("S_C", "Side_A", "SideJoin"),
            ("S_D", "Side_B", "SideJoin"),
            ("S_E", "SideJoin", "End_1"),
        ] {
            model.sequence_flows.push(edge(id, source, target));
        }
        let started = start_at(&fixture, &model, 1_000);
        for (offset, node_id) in ["Main_A", "Main_B"].iter().enumerate() {
            let snapshot =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            assert!(snapshot.timers.is_empty());
            let task = snapshot
                .user_tasks
                .iter()
                .find(|task| task.node_id == *node_id)
                .unwrap();
            complete_work(
                &fixture,
                &started.instance_id,
                &task.user_task_id,
                2_000 + offset as i64,
            );
        }
        let attached =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(attached.tokens.len(), 1);
        assert!(attached.tokens[0].fork_stack.is_empty());
        let due = attached.timers[0].due_at_ms.unwrap();
        let drained = drain_due(&fixture.db, due);
        drained.completion.unwrap();
        assert_eq!(drained.fired, 1);
        let side = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
            .unwrap();
        assert_eq!(side.tokens.len(), 2);
        assert!(side
            .tokens
            .iter()
            .all(|token| token.fork_stack.len() == 1
                && token.fork_stack[0].split_node_id == "SideSplit"));
        assert_eq!(
            side.user_tasks
                .iter()
                .find(|task| task.node_id == "Work")
                .unwrap()
                .status,
            ProcessUserTaskStatus::Cancelled
        );
        for node_id in ["Side_A", "Side_B"] {
            let snapshot =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            let task = snapshot
                .user_tasks
                .iter()
                .find(|task| task.node_id == node_id)
                .unwrap();
            complete_work(&fixture, &started.instance_id, &task.user_task_id, due + 1);
        }
        let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
            .unwrap();
        assert_eq!(after.instance.status, ProcessInstanceStatus::Completed);
        assert!(after.tokens.is_empty() && after.receipts.is_empty());
        let events =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "parallel_joined")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn boundary_arming_horizon_error_keeps_waiting_activation_and_resolves_on_completion() {
        let fixture = Fixture::new();
        let model = with_boundaries(user_model(None), "Work", &[("TooFar", true, 1)]);
        let horizon = chrono::DateTime::<Utc>::MAX_UTC.timestamp_millis();
        let started = start_at(&fixture, &model, horizon);
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Incident);
        assert_eq!(snapshot.timers[0].status, ProcessTimerStatus::Error);
        assert_eq!(snapshot.timers[0].due_at_ms, None);
        assert_eq!(snapshot.boundary_incidents.len(), 1);
        assert_eq!(snapshot.user_tasks[0].status, ProcessUserTaskStatus::Open);
        assert!(repository::due_timers(&fixture.db, horizon, 32)
            .unwrap()
            .is_empty());
        let completed = complete_work(
            &fixture,
            &started.instance_id,
            &snapshot.user_tasks[0].user_task_id,
            horizon,
        );
        assert_eq!(completed.status, ProcessInstanceStatus::Completed);
        assert!(completed.incidents.is_empty());
        assert_eq!(completed.timers[0].status, ProcessTimerStatus::Error);
    }

    #[tokio::test]
    async fn boundary_service_condition_and_human_approval_disarm_only_on_actual_completion() {
        for human in [false, true] {
            let fixture = Fixture::new();
            let flow_id = flow(
                &fixture.db,
                &fixture.owner,
                &graph("immutable service result", None),
            );
            let model = with_boundaries(
                service_model(
                    &flow_id,
                    if human {
                        ActivityVerification::Human
                    } else {
                        ActivityVerification::Condition {
                            expression: "true".into(),
                        }
                    },
                ),
                "Service",
                &[("Limit", true, 30)],
            );
            let started = start_model(&fixture, &model);
            let claim = repository::claim_job(
                &fixture.db,
                "completed-service",
                Utc::now().timestamp_millis(),
            )
            .unwrap()
            .unwrap();
            super::super::jobs::execute_claimed(
                &fixture.db,
                fixture.dispatcher(),
                "completed-service",
                claim,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
            let snapshot =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            let completed = if human {
                assert_eq!(snapshot.timers[0].status, ProcessTimerStatus::Pending);
                let task = &snapshot.user_tasks[0];
                let at_ms = Utc::now().timestamp_millis();
                let plan = runtime::plan_user_completion(
                    &snapshot,
                    &task.user_task_id,
                    &json!({}),
                    Some(true),
                    at_ms,
                )
                .unwrap();
                repository::complete_user_task(
                    &fixture.db,
                    &fixture.owner,
                    &stamp("approve immutable service result"),
                    &started.instance_id,
                    &task.user_task_id,
                    snapshot.instance.revision,
                    &json!({}),
                    Some(true),
                    &plan,
                    at_ms,
                )
                .unwrap()
            } else {
                snapshot.instance
            };
            assert_eq!(completed.status, ProcessInstanceStatus::Completed);
            assert_eq!(completed.variables["answer"], "immutable service result");
            assert_eq!(completed.timers[0].status, ProcessTimerStatus::Cancelled);
            assert_eq!(
                completed.timers[0].last_reason.as_deref(),
                Some("activity_completed")
            );
        }
    }
}
