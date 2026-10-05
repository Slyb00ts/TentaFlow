// ============ File: timers.rs — durable process timer slot arithmetic and transition planning ============

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Days, Duration, LocalResult, NaiveDate, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::json;
use tentaflow_protocol::processes::{
    ProcessCalendarPin, ProcessTimerKind, ProcessTimerSpec, ProcessTimerStatus,
};
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
    calendar_pin: Option<&ProcessCalendarPin>,
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
        ProcessTimerSpec::WorkingDuration { seconds } => {
            let pin = calendar_pin.context("WorkingDuration requires an immutable calendar pin")?;
            ensure!(
                pin.timezone_data.iana_name == zone,
                "working timer timezone differs from its immutable pin"
            );
            super::calendar::working_due(pin, anchor_at_ms, *seconds)
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

fn due_for_occurrence(
    timer: &ProcessTimer,
    occurrence: u64,
    calendar_pin: Option<&ProcessCalendarPin>,
) -> Result<i64> {
    checked_occurrence(occurrence)?;
    match &timer.rule {
        ProcessTimerSpec::Cycle { seconds, .. } => {
            resolve_timer_due(
                &timer.rule,
                &timer.timezone,
                timer.kind.clone(),
                timer.anchor_at_ms,
                calendar_pin,
            )?;
            elapsed_due(timer.anchor_at_ms, *seconds, occurrence)
        }
        ProcessTimerSpec::Daily { hour, minute, .. } => {
            let first = resolve_timer_due(
                &timer.rule,
                &timer.timezone,
                timer.kind.clone(),
                timer.anchor_at_ms,
                calendar_pin,
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
                calendar_pin,
            )
        }
    }
}

pub fn next_timer_occurrence(
    timer: &ProcessTimer,
    at_ms: i64,
    mode: TimerAdvanceMode,
    calendar_pin: Option<&ProcessCalendarPin>,
) -> Result<TimerAdvance> {
    instant(at_ms)?;
    checked_occurrence(timer.occurrence)?;
    let due = timer
        .due_at_ms
        .context("timer has no pending due instant")?;
    ensure!(
        due_for_occurrence(timer, timer.occurrence, calendar_pin)? == due,
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
                calendar_pin,
            )?;
            let first_date = local_date(instant(first)?, tz)?;
            let current_date = local_date(instant(at_ms)?, tz)?;
            let days = (current_date - first_date).num_days();
            let guess = u64::try_from(days)
                .context("daily timer precedes its first slot")?
                .checked_add(1)
                .context("daily timer slot overflow")?;
            let guess = total.map_or(guess, |count| guess.min(u64::from(count)));
            if due_for_occurrence(timer, guess, calendar_pin)? > at_ms {
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
    let planned_due_at_ms = due_for_occurrence(timer, selected, calendar_pin)?;
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
    let next_due = due_for_occurrence(timer, next_occurrence, calendar_pin)?;
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

pub fn plan_timer_fire(snapshot: &TimerSnapshot, at_ms: i64,
    signal_admission: Option<&runtime::SignalAdmissionResolver<'_>>,
) -> Result<RuntimePlan> {
    let timer = match snapshot {
        TimerSnapshot::Start { timer, .. }
        | TimerSnapshot::Catch { timer, .. }
        | TimerSnapshot::Boundary { timer, .. } => timer,
    };
    let model = match snapshot {
        TimerSnapshot::Start { version, .. } => &version.model,
        TimerSnapshot::Catch { snapshot, .. } | TimerSnapshot::Boundary { snapshot, .. } => {
            &snapshot.model
        }
    };
    let advance = next_timer_occurrence(
        timer,
        at_ms,
        TimerAdvanceMode::Fire,
        model.calendar_pin.as_ref(),
    )?;
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
                repository::StartInputRef::Timer {
                    timer_id: timer.timer_id.clone(),
                    expected_timer_revision: timer.revision,
                    fired_occurrence: advance.selected_occurrence,
                },
            signal_admission)?
        }
        TimerSnapshot::Catch {
            timer, snapshot, ..
        } => runtime::plan_timer_catch(snapshot, timer, at_ms,
            repository::AcceptedInputRef::Timer {
                timer_id: timer.timer_id.clone(),
                expected_timer_revision: timer.revision,
                fired_occurrence: advance.selected_occurrence,
            }, signal_admission)?,
        TimerSnapshot::Boundary {
            timer, snapshot, ..
        } => runtime::plan_timer_boundary(snapshot, timer, at_ms,
            repository::AcceptedInputRef::Timer {
                timer_id: timer.timer_id.clone(),
                expected_timer_revision: timer.revision,
                fired_occurrence: advance.selected_occurrence,
            }, signal_admission)?,
    };
    let skipped_from = (advance.skipped_count > 0).then_some(timer.occurrence);
    let skipped_through = (advance.skipped_count > 0).then(|| advance.selected_occurrence - 1);
    let mut event = PlannedEvent {
        scope_id: timer
            .scope_id
            .clone()
            .or_else(|| plan.start_instance_id.clone())
            .context("timer fire scope missing")?,
        kind: "timer_fired".into(),
        node_id: Some(timer.node_id.clone()),
        data: json!({"timer_id":timer.timer_id,"kind":timer.kind,"occurrence":advance.selected_occurrence,
            "planned_due_at_ms":advance.planned_due_at_ms,"fired_at_ms":at_ms,"skipped_count":advance.skipped_count,
            "skipped_from_occurrence":skipped_from,"skipped_through_occurrence":skipped_through}),
    };
    if let Some(working_time) = super::calendar::working_time_summary(
        &timer.rule,
        model.calendar_pin.as_ref(),
        Some(advance.planned_due_at_ms),
    )? {
        event.data["working_time"] = serde_json::to_value(working_time)?;
        event.data["timezone"] = json!(timer.timezone);
    }
    if let TimerSnapshot::Boundary { snapshot, .. } = snapshot {
        let node = super::repository::scope_node(
            &snapshot.model,
            &snapshot.scopes,
            &snapshot.instance.instance_id,
            timer
                .scope_id
                .as_deref()
                .context("boundary timer scope missing")?,
            &timer.node_id,
        )?;
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
        plan.event_sources = std::mem::take(&mut plan.event_sources).into_iter()
            .map(|(index, token_id)| (index + 1, token_id)).collect();
        for index in plan.event_ids.keys().rev().cloned().collect::<Vec<_>>() {
            if let Some(event_id) = plan.event_ids.remove(&index) {
                plan.event_ids.insert(index + 1, event_id);
            }
        }
        for attempt in &mut plan.termination_attempts {
            match attempt {
                repository::TerminationAttempt::Success(source) => source.source_event_index += 1,
                repository::TerminationAttempt::ReturnFailure(failure) => failure.source_event_index += 1,
            }
        }
        for effect in &mut plan.variable_effects {
            match effect {
                repository::VariableEffect::Mapped { event_index, .. }
                | repository::VariableEffect::ScopeEntry { event_index, .. }
                | repository::VariableEffect::RepetitionEntry { event_index, .. }
                | repository::VariableEffect::RepetitionAggregate { event_index, .. } => *event_index += 1,
            }
        }
        for message in &mut plan.create_messages {
            message.source_event_index += 1;
        }
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
            let (actor, revision) = match &snapshot {
                TimerSnapshot::Start { actor, .. } => (actor, None),
                TimerSnapshot::Catch {
                    actor, snapshot, ..
                }
                | TimerSnapshot::Boundary {
                    actor, snapshot, ..
                } => (actor, Some(snapshot.instance.revision)),
            };
            repository::fire_timer(pool, &candidate, actor, revision,
                repository::ProcessPlanInput::Canonical, at_ms)
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
        ActivityVerification, ProcessInstance, ProcessInstanceStatus, ProcessMessageDeclaration,
        ProcessMessageStatus, ProcessMessageTargetSpec, ProcessModel, ProcessNode,
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
        let due = resolve_timer_due(&rule, zone, ProcessTimerKind::Start, anchor, None).unwrap();
        ProcessTimer {
            scope_id: None,
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
            race_id: None,
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
                repeat: None,
            },
        );
        model.sequence_flows = vec![
            edge("ToWait", "Start_1", "Wait"),
            edge("ToEnd", "Wait", "End_1"),
        ];
        model
    }

    #[test]
    fn boundary_timer_termination_preserves_local_event_and_mapping_indices() {
        let fixture = Fixture::new();
        let receiver = super::super::messages::test_support::published(
            &fixture,
            &super::super::messages::test_support::receiving_model(true, false),
        );
        let mut model = with_boundaries(user_model(None), "Work", &[("Limit", true, 1)]);
        model.messages.push(ProcessMessageDeclaration {
            message_id: "ThrowDecl".into(),
            name: "EvidenceReady".into(),
        });
        model.nodes.push(ProcessNode {
            id: "Throw".into(),
            name: "Queue evidence".into(),
            kind: ProcessNodeKind::MessageThrow {
                message_ref: "ThrowDecl".into(),
                target: ProcessMessageTargetSpec::Start {
                    definition_id: receiver.definition_id,
                },
                correlation_expression: "'case-1'".into(),
                payload_expression: "{'customer_ID': 23}".into(),
                ttl_seconds: 120,
            },
            repeat: None,
        });
        let body = tentaflow_protocol::processes::ProcessSubProcess {
            nodes: vec![
                ProcessNode { id: "ChildStart".into(), name: "Enter child".into(),
                    kind: ProcessNodeKind::Start,
                    repeat: None, },
                ProcessNode { id: "ChildTerminate".into(), name: "Terminate child".into(),
                    kind: ProcessNodeKind::TerminateEnd,
                    repeat: None, },
            ],
            sequence_flows: vec![edge("ChildToTerminate", "ChildStart", "ChildTerminate")],
            variables: BTreeMap::new(),
            diagram: Default::default(),
        };
        model.nodes.push(ProcessNode { id: "Scope".into(), name: "Timer child".into(),
            kind: ProcessNodeKind::SubProcess { body,
                input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new() },
            repeat: None, });
        model.sequence_flows.iter_mut().find(|flow| flow.id == "From_Limit")
            .unwrap().target_id = "Throw".into();
        model.sequence_flows.push(edge("ThrowScope", "Throw", "Scope"));
        model.sequence_flows.push(edge("ScopeToEnd", "Scope", "End_1"));
        let anchor = Utc::now().timestamp_millis();
        let started = start_at(&fixture, &model, anchor);
        let before = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let due = before.timers.iter().find(|timer| timer.node_id == "Limit")
            .unwrap().due_at_ms.unwrap();
        let candidate = repository::due_timers(&fixture.db, due, 32).unwrap()
            .into_iter().find(|timer| timer.timer_id == before.timers[0].timer_id).unwrap();
        let snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
        let plan = plan_timer_fire(&snapshot, due, None).unwrap();
        assert_eq!(plan.events[0].kind, "timer_fired");
        assert_eq!(plan.create_messages.len(), 1);
        let queued_index = plan.create_messages[0].source_event_index;
        assert!(queued_index > 0);
        assert_eq!(plan.events[queued_index].kind, "message_queued");
        assert!(plan.event_sources.contains_key(&queued_index));
        assert!(!plan.event_ids.contains_key(&queued_index));
        assert!(plan.events.iter().any(|event| event.kind == "terminate_end_reached"));
        assert!(plan.event_sources.iter().all(|(index, token_id)|
            *index > 0 && plan.events.get(*index).is_some_and(|event|
                event.node_id.is_some() && !token_id.is_empty())));
        assert!(plan.variable_effects.iter().any(|effect| match effect {
            repository::VariableEffect::Mapped { event_index, .. }
            | repository::VariableEffect::ScopeEntry { event_index, .. }
            | repository::VariableEffect::RepetitionEntry { event_index, .. }
            | repository::VariableEffect::RepetitionAggregate { event_index, .. } => *event_index > 0,
        }));
        let committed = repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(before.instance.revision), repository::ProcessPlanInput::Supplied(&plan), due).unwrap().unwrap().instance;
        assert_eq!(committed.status, ProcessInstanceStatus::Completed);
        assert_eq!(committed.outgoing_messages.len(), 1);
        assert_eq!(committed.outgoing_messages[0].status, ProcessMessageStatus::Pending);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.instance.status, ProcessInstanceStatus::Completed);
        assert_eq!(persisted.instance.variables, committed.variables);
        let events = repository::list_events(&reopened, &fixture.owner,
            &started.instance_id, 0, 200).unwrap().0;
        let queued_event = events.iter().find(|event| event.kind == "message_queued").unwrap();
        let source_event_id: String = reopened.read().unwrap().query_row(
            "SELECT source_event_id FROM bpmn_messages WHERE message_id=?1",
            [committed.outgoing_messages[0].message_id.as_str()],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(source_event_id, queued_event.event_id);
        assert_eq!(events.iter()
            .filter(|event| event.kind == "terminate_end_reached").count(), 1);
    }

    fn start_at(fixture: &Fixture, model: &ProcessModel, at_ms: i64) -> ProcessInstance {
        let version = publish_model(fixture, model);
        let id = Uuid::new_v4().to_string();
        let variables = serde_json::to_value(&model.variables).unwrap();
        let command = stamp("timed manual start");
        let plan = runtime::plan_start(
            &version.model,
            &id,
            &fixture.owner,
            &version.definition_id,
            version.version,
            variables.clone(),
            StartCause::Manual,
            at_ms,
            runtime::test_support::manual_input(&command),
        None)
        .unwrap();
        repository::start_instance(
            &fixture.db,
            &fixture.owner,
            &command,
            &id,
            &version.definition_id,
            version.version,
            &variables,
            repository::ProcessPlanInput::Supplied(&plan),
            at_ms,
        )
        .unwrap()
    }

    #[test]
    fn timer_race_winner_reaching_terminate_closes_only_losing_catch() {
        let fixture = Fixture::new();
        let mut model = super::super::messages::test_support::race_model();
        model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
            ProcessNodeKind::TerminateEnd;
        let anchor = Utc::now().timestamp_millis() - 3_000;
        let started = start_at(&fixture, &model, anchor);
        let other = start_at(&fixture, &model, anchor + 60_000);
        let due = started.timers[0].due_at_ms.unwrap();
        let candidate = repository::due_timers(&fixture.db, due, 32).unwrap().into_iter()
            .find(|candidate| candidate.timer_id == started.timers[0].timer_id).unwrap();
        let snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
        let plan = plan_timer_fire(&snapshot, due, None).unwrap();
        use tentaflow_protocol::processes::{ProcessEventRaceStatus as R, ProcessSubscriptionStatus as S};
        assert_eq!(plan.race_updates.len(), 1);
        assert_eq!(plan.race_updates[0].status, R::Won);
        assert_eq!(plan.events.iter().filter(|event| event.kind == "event_race_won").count(), 1);
        assert_eq!(plan.events.iter().filter(|event| event.kind == "event_race_cancelled").count(), 0);
        assert_eq!(plan.subscription_updates.iter().filter(|update| update.status == S::Cancelled).count(), 1);
        let before = super::super::call_tests::transition_rows(&fixture);
        let mut duplicate = plan.clone();
        let mut extra = duplicate.race_updates[0].clone();
        extra.status = R::Cancelled;
        extra.winner_node_id = None;
        extra.winner_subscription_id = None;
        extra.winner_timer_id = None;
        duplicate.race_updates.push(extra);
        assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(started.revision), repository::ProcessPlanInput::Supplied(&duplicate), due).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&fixture), before);
        let mut wrong_winner = plan.clone();
        wrong_winner.race_updates[0].winner_node_id = Some("Catch_1".into());
        assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(started.revision), repository::ProcessPlanInput::Supplied(&wrong_winner), due).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&fixture), before);
        let mut foreign = plan.clone();
        let other_tokens = repository::runtime_snapshot(&fixture.db, &fixture.owner, &other.instance_id).unwrap().tokens;
        foreign.cancel_token_ids.push(other_tokens[0].token_id.clone());
        assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(started.revision), repository::ProcessPlanInput::Supplied(&foreign), due).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&fixture), before);
        let drained = drain_due(&fixture.db, due);
        drained.completion.unwrap();
        assert_eq!(drained.fired, 1);
        let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(after.instance.status, ProcessInstanceStatus::Completed);
        assert_eq!(after.event_races[0].status, R::Won);
        assert_eq!(after.timers[0].status, ProcessTimerStatus::Fired);
        assert_eq!(after.subscriptions[0].status, S::Cancelled);
        let events = repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200).unwrap().0;
        assert_eq!(events.iter().filter(|event| event.kind == "event_race_won").count(), 1);
        assert_eq!(events.iter().filter(|event| event.kind == "event_race_cancelled").count(), 0);
        assert_eq!(events.iter().filter(|event| event.kind == "subscription_cancelled").count(), 1);
        assert_eq!(events.iter().filter(|event| event.kind == "terminate_end_reached").count(), 1);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        assert_eq!(repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap().event_races[0].status, R::Won);
        let replay = drain_due(&reopened, due + 1);
        replay.completion.unwrap();
        assert_eq!(replay.fired, 0);
    }

    #[test]
    fn timer_race_termination_closes_unrelated_waiting_user_task() {
        let fixture = Fixture::new();
        let mut model = super::super::messages::test_support::race_model();
        model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
            ProcessNodeKind::TerminateEnd;
        model.nodes.extend([
            ProcessNode { id: "Split".into(), name: "Concurrent paths".into(),
                kind: ProcessNodeKind::ParallelGateway,
                repeat: None, },
            ProcessNode { id: "SideWork".into(), name: "Independent open work".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None,
                    output_mapping: BTreeMap::new() },
                repeat: None, },
        ]);
        model.sequence_flows = vec![
            edge("StartSplit", "Start_1", "Split"),
            edge("SplitRace", "Split", "Race_1"),
            edge("SplitSide", "Split", "SideWork"),
            edge("RaceMessage", "Race_1", "Catch_1"),
            edge("RaceTimer", "Race_1", "Timer_1"),
            edge("MessageEnd", "Catch_1", "End_1"),
            edge("TimerEnd", "Timer_1", "End_1"),
            edge("SideEnd", "SideWork", "End_1"),
        ];
        let anchor = Utc::now().timestamp_millis() - 3_000;
        let started = start_at(&fixture, &model, anchor);
        assert_eq!(started.user_tasks.len(), 1);
        assert_eq!(started.user_tasks[0].status, ProcessUserTaskStatus::Open);
        let due = started.timers[0].due_at_ms.unwrap();
        let candidate = repository::due_timers(&fixture.db, due, 32).unwrap().into_iter()
            .find(|candidate| candidate.timer_id == started.timers[0].timer_id).unwrap();
        let snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
        let plan = plan_timer_fire(&snapshot, due, None).unwrap();
        assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(started.revision), repository::ProcessPlanInput::Supplied(&plan), due).unwrap().is_some());
        let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(after.instance.status, ProcessInstanceStatus::Completed);
        assert_eq!(after.event_races[0].status, tentaflow_protocol::processes::ProcessEventRaceStatus::Won);
        assert_eq!(after.user_tasks[0].status, ProcessUserTaskStatus::Cancelled);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        assert_eq!(repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap().user_tasks[0].status,
            ProcessUserTaskStatus::Cancelled);
        let replay = drain_due(&reopened, due + 1);
        replay.completion.unwrap();
        assert_eq!(replay.fired, 0);
    }

    #[test]
    fn timer_race_termination_closes_a_distinct_open_race_with_source_proof() {
        let fixture = Fixture::new();
        let model = super::super::messages::test_support::parallel_races_reaching_terminate();
        let anchor = Utc::now().timestamp_millis() - 3_000;
        let started = start_at(&fixture, &model, anchor);
        assert_eq!(started.event_races.len(), 2);
        let first = started.event_races.iter().find(|race| race.gateway_node_id == "Race_1").unwrap();
        let other = started.event_races.iter().find(|race| race.gateway_node_id == "OtherRace").unwrap();
        let due = started.timers.iter().find(|timer| timer.node_id == "Timer_1").unwrap().due_at_ms.unwrap();
        let candidate = repository::due_timers(&fixture.db, due, 32).unwrap().into_iter()
            .find(|candidate| candidate.timer_id == started.timers.iter()
                .find(|timer| timer.node_id == "Timer_1").unwrap().timer_id).unwrap();
        let snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
        let plan = plan_timer_fire(&snapshot, due, None).unwrap();
        use tentaflow_protocol::processes::ProcessEventRaceStatus as R;
        assert_eq!(plan.race_updates.len(), 2);
        assert!(plan.race_updates.iter().any(|update| update.race_id == first.race_id && update.status == R::Won));
        assert!(plan.race_updates.iter().any(|update| update.race_id == other.race_id && update.status == R::Cancelled));
        let before = super::super::call_tests::transition_rows(&fixture);
        let loser = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap()
            .subscriptions.into_iter().find(|sub| sub.node_id == "Catch_1").unwrap().token_id;
        let mut missing_loser = plan.clone();
        missing_loser.cancel_token_ids.retain(|id| id != &loser);
        assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(started.revision), repository::ProcessPlanInput::Supplied(&missing_loser), due).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&fixture), before);
        let mut duplicate_loser = plan.clone();
        duplicate_loser.cancel_token_ids.push(loser);
        assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(started.revision), repository::ProcessPlanInput::Supplied(&duplicate_loser), due).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&fixture), before);
        let mut wrong_scope = plan.clone();
        wrong_scope.events.iter_mut().find(|event| event.kind == "event_race_cancelled"
            && event.data["race_id"].as_str() == Some(other.race_id.as_str())).unwrap().scope_id =
            Uuid::new_v4().to_string();
        assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(started.revision), repository::ProcessPlanInput::Supplied(&wrong_scope), due).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&fixture), before);
        let mut extra_winner = plan.clone();
        let other_update = extra_winner.race_updates.iter_mut().find(|update| update.race_id == other.race_id).unwrap();
        other_update.status = R::Won;
        other_update.winner_node_id = Some("OtherCatch".into());
        other_update.winner_subscription_id = started.subscriptions.iter().find(|sub| sub.node_id == "OtherCatch")
            .map(|sub| sub.subscription_id.clone());
        assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(started.revision), repository::ProcessPlanInput::Supplied(&extra_winner), due).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&fixture), before);
        assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(started.revision), repository::ProcessPlanInput::Supplied(&plan), due).unwrap().is_some());
        let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(after.instance.status, ProcessInstanceStatus::Completed);
        assert_eq!(after.event_races.iter().find(|race| race.race_id == first.race_id).unwrap().status, R::Won);
        assert_eq!(after.event_races.iter().find(|race| race.race_id == other.race_id).unwrap().status, R::Cancelled);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let history = repository::list_events(&reopened, &fixture.owner, &started.instance_id, 0, 200).unwrap().0;
        assert_eq!(history.iter().filter(|event| event.kind == "event_race_won").count(), 1);
        assert_eq!(history.iter().filter(|event| event.kind == "event_race_cancelled").count(), 1);
        let replay = drain_due(&reopened, due + 1);
        replay.completion.unwrap();
        assert_eq!(replay.fired, 0);
    }

    #[test]
    fn due_rules_require_explicit_timezone_and_preserve_elapsed_and_absolute_instants() {
        let anchor = at("2026-03-29T00:30:00Z");
        let date = ProcessTimerSpec::Date {
            at: "2026-03-29T03:30:00+02:00".into(),
        };
        assert_eq!(
            resolve_timer_due(
                &date,
                "Europe/Warsaw",
                ProcessTimerKind::Start,
                anchor,
                None
            )
            .unwrap(),
            anchor + 3_600_000
        );
        assert!(resolve_timer_due(&date, "", ProcessTimerKind::Catch, anchor, None).is_err());
        assert!(
            resolve_timer_due(&date, "Mars/Unknown", ProcessTimerKind::Catch, anchor, None)
                .is_err()
        );
        assert!(resolve_timer_due(
            &date,
            "UTC",
            ProcessTimerKind::Start,
            anchor + 3_600_000,
            None
        )
        .is_err());
        assert_eq!(
            resolve_timer_due(
                &date,
                "UTC",
                ProcessTimerKind::Catch,
                anchor + 7_200_000,
                None
            )
            .unwrap(),
            anchor + 3_600_000
        );
        assert!(resolve_timer_due(
            &ProcessTimerSpec::Date {
                at: "2026-03-29T01:30:00.0001Z".into()
            },
            "UTC",
            ProcessTimerKind::Catch,
            anchor,
            None
        )
        .is_err());
        assert_eq!(
            resolve_timer_due(
                &ProcessTimerSpec::Duration { seconds: 7200 },
                "Europe/Warsaw",
                ProcessTimerKind::Catch,
                anchor,
                None
            )
            .unwrap(),
            anchor + 7_200_000
        );
        assert!(resolve_timer_due(
            &ProcessTimerSpec::Duration { seconds: 0 },
            "UTC",
            ProcessTimerKind::Catch,
            anchor,
            None
        )
        .is_err());
        assert!(resolve_timer_due(
            &ProcessTimerSpec::Cycle {
                seconds: 300,
                total_firings: None
            },
            "UTC",
            ProcessTimerKind::Catch,
            anchor,
            None
        )
        .is_err());
        assert!(resolve_timer_due(
            &ProcessTimerSpec::Duration { seconds: 1 },
            "UTC",
            ProcessTimerKind::Catch,
            DateTime::<Utc>::MAX_UTC.timestamp_millis(),
            None
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
            next_timer_occurrence(&finite, anchor + 1_005_000, TimerAdvanceMode::Fire, None)
                .unwrap();
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
            next_timer_occurrence(&unlimited, anchor + 1_005_000, TimerAdvanceMode::Fire, None)
                .unwrap();
        assert_eq!(advance.next_occurrence, 4);
        assert_eq!(advance.next_due_at_ms, Some(anchor + 1_200_000));
        let restore = next_timer_occurrence(
            &unlimited,
            anchor + 1_005_000,
            TimerAdvanceMode::Restore,
            None,
        )
        .unwrap();
        assert_eq!(restore.skipped_count, 3);
        assert_eq!(restore.next_due_at_ms, Some(anchor + 1_200_000));
        assert!(elapsed_due(anchor, 300, u64::MAX).is_err());
        assert!(next_timer_occurrence(
            &unlimited,
            DateTime::<Utc>::MAX_UTC.timestamp_millis(),
            TimerAdvanceMode::Fire,
            None
        )
        .is_err());
        let one_shot = timer(ProcessTimerSpec::Duration { seconds: 30 }, "UTC", anchor);
        assert_eq!(
            next_timer_occurrence(&one_shot, anchor + 60_000, TimerAdvanceMode::Restore, None)
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
            due_for_occurrence(&spring, 2, None).unwrap(),
            at("2026-03-29T01:30:00Z")
        );
        assert_eq!(
            due_for_occurrence(&spring, 3, None).unwrap(),
            at("2026-03-30T00:30:00Z")
        );
        let fired = next_timer_occurrence(
            &spring,
            at("2026-03-30T10:00:00Z"),
            TimerAdvanceMode::Fire,
            None,
        )
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
            due_for_occurrence(&autumn, 2, None).unwrap(),
            at("2026-10-25T00:30:00Z")
        );
        assert_eq!(
            due_for_occurrence(&autumn, 3, None).unwrap(),
            at("2026-10-26T01:30:00Z")
        );
        let restore = next_timer_occurrence(
            &autumn,
            at("2026-10-25T01:00:00Z"),
            TimerAdvanceMode::Restore,
            None,
        )
        .unwrap();
        assert_eq!(restore.skipped_count, 2);
        assert_eq!(restore.next_occurrence, 3);
        assert_eq!(restore.next_due_at_ms, Some(at("2026-10-26T01:30:00Z")));
        assert!(due_for_occurrence(&autumn, i64::MAX as u64, None).is_err());
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
        let plan = plan_timer_fire(&snapshot, at_ms, None).unwrap();
        let id = plan
            .start_instance_id
            .clone()
            .expect("a Start to End plan retains its actual UUID");
        assert!(plan.create_jobs.is_empty() && plan.create_user_tasks.is_empty());
        assert!(plan.create_scopes.is_empty());
        assert_eq!(plan.status, ProcessInstanceStatus::Completed);
        assert_eq!(plan.create_tokens.len(), model.nodes.len());
        assert!(plan.create_tokens.iter().all(|token| {
            token.scope_id == id
                && token.status == "ready"
                && model.nodes.iter().any(|node| node.id == token.node_id)
        }));
        assert_eq!(
            plan.create_tokens
                .iter()
                .map(|token| &token.token_id)
                .collect::<std::collections::HashSet<_>>(),
            plan.consume_token_ids
                .iter()
                .collect::<std::collections::HashSet<_>>()
        );
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
        let other_plan = plan_timer_fire(&other_snapshot, at_ms, None).unwrap();
        let other_id = other_plan.start_instance_id.clone().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let first_barrier = barrier.clone();
            let (pool, first_candidate, actor, first_plan) = (&reopened, &candidate, &owner, &plan);
            let first = scope.spawn(move || {
                first_barrier.wait();
                repository::fire_timer(pool, first_candidate, actor, None, repository::ProcessPlanInput::Supplied(first_plan), at_ms)
                    .unwrap()
            });
            let second = scope.spawn(|| {
                barrier.wait();
                repository::fire_timer(
                    &reopened,
                    &other_candidate,
                    &owner,
                    None,
                    repository::ProcessPlanInput::Supplied(&other_plan),
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
        assert_eq!(first.scopes.len(), 1);
        assert_eq!(first.scopes[0].scope_id, first.instance_id);
        let retained_tokens: Vec<(String, String, String)> = {
            let conn = reopened.read().unwrap();
            let mut query = conn
                .prepare("SELECT scope_id,node_id,status FROM bpmn_tokens WHERE instance_id=?1 ORDER BY node_id")
                .unwrap();
            let rows = query
                .query_map([&first.instance_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            rows
        };
        assert_eq!(retained_tokens.len(), model.nodes.len());
        assert!(retained_tokens.iter().all(|(scope, node, status)| {
            scope == &first.instance_id
                && status == "consumed"
                && model.nodes.iter().any(|actual| &actual.id == node)
        }));
        assert!(
            repository::fire_timer(&reopened, &candidate, &owner, None, repository::ProcessPlanInput::Supplied(&plan), at_ms)
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
        let plan = plan_timer_fire(&timer_snapshot, at_ms + 60_000, None).unwrap();
        repository::cancel_instance(
            &reopened,
            &owner,
            &stamp("cancel before timer commit"),
            &waiting.instance_id,
            waiting.revision,
        )
        .unwrap()
        .instance;
        assert!(repository::fire_timer(
            &reopened,
            &candidate,
            &owner,
            Some(waiting.revision),
            repository::ProcessPlanInput::Supplied(&plan),
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
        let cancelled =
            repository::get_instance(&reopened, &owner, &waiting.instance_id, None).unwrap();
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
            repository::get_instance(&fixture.db, &fixture.owner, &past.instance_id, None)
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
                repeat: None,
            },
            ProcessNode {
                id: "Review".into(),
                name: "Independent review".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
                repeat: None,
            },
            ProcessNode {
                id: "Join".into(),
                name: "Wait for both branches".into(),
                kind: ProcessNodeKind::ParallelGateway,
                repeat: None,
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
        let plan = plan_timer_fire(&snapshot, at_ms + 60_000, None).unwrap();
        let partial = repository::fire_timer(
            &fixture.db,
            &candidate,
            &fixture.owner,
            Some(waiting.revision),
            repository::ProcessPlanInput::Supplied(&plan),
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
        let command = stamp("review after due");
        let plan = runtime::plan_user_completion(
            &snapshot,
            &task.user_task_id,
            &json!({}),
            None,
            at_ms + 61_000,
            runtime::test_support::human_input(&snapshot, &task.user_task_id, &command),
        None)
        .unwrap();
        let completed = repository::complete_user_task(
            &reopened,
            &owner,
            &command,
            &waiting.instance_id,
            &task.user_task_id,
            partial.revision,
            &json!({}),
            None,
            repository::ProcessPlanInput::Supplied(&plan),
            at_ms + 61_000,
        )
        .unwrap()
        .instance;
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
            repository::ProcessPlanInput::Supplied(&plan),
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
                repeat: None,
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
            repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id, None)
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
        let plan = plan_timer_fire(&snapshot, due + 120_000, None).unwrap();
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
            repository::ProcessPlanInput::Supplied(&plan),
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
            repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id, None)
                .is_err()
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
                repeat: None,
            },
            ProcessNode {
                id: "Review".into(),
                name: "Independent review".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
                repeat: None,
            },
            ProcessNode {
                id: "Join".into(),
                name: "Join both".into(),
                kind: ProcessNodeKind::ParallelGateway,
                repeat: None,
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
            None,
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
        let command = stamp("finish unaffected branch");
        let plan = runtime::plan_user_completion(
            &snapshot,
            &task.user_task_id,
            &json!({}),
            None,
            at_ms + 61_000,
            runtime::test_support::human_input(&snapshot, &task.user_task_id, &command),
        None)
        .unwrap();
        let still_incident = repository::complete_user_task(
            &fixture.db,
            &fixture.owner,
            &command,
            &waiting.instance_id,
            &task.user_task_id,
            snapshot.instance.revision,
            &json!({}),
            None,
            repository::ProcessPlanInput::Supplied(&plan),
            at_ms + 61_000,
        )
        .unwrap()
        .instance;
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
                repeat: None,
            },
            ProcessNode {
                id: "After".into(),
                name: "Wait after service result".into(),
                kind: ProcessNodeKind::TimerCatch {
                    timer: ProcessTimerSpec::Duration { seconds: 1 },
                },
                repeat: None,
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
            repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id, None)
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
                repeat: None,
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
            repository::get_instance(&fixture.db, &fixture.owner, &future.instance_id, None)
                .unwrap();
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
            repository::get_instance(&fixture.db, &fixture.owner, &future.instance_id, None)
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
            let command = stamp("bounded timer start");
            let plan = runtime::plan_start(
                &model,
                &id,
                &fixture.owner,
                &version.definition_id,
                version.version,
                json!({}),
                StartCause::Manual,
                at_ms,
                runtime::test_support::manual_input(&command),
            None)
            .unwrap();
            repository::start_instance(
                &fixture.db,
                &fixture.owner,
                &command,
                &id,
                &version.definition_id,
                version.version,
                &json!({}),
                repository::ProcessPlanInput::Supplied(&plan),
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
        let plan = plan_timer_fire(&snapshot, at_ms, None).unwrap();
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
        let command = stamp("complete exact work");
        let plan =
            runtime::plan_user_completion(&snapshot, task_id, &outputs, None, at_ms,
                runtime::test_support::human_input(&snapshot, task_id, &command), None).unwrap();
        repository::complete_user_task(
            &fixture.db,
            actor,
            &command,
            instance_id,
            task_id,
            snapshot.instance.revision,
            &outputs,
            None,
            repository::ProcessPlanInput::Supplied(&plan),
            at_ms,
        )
        .unwrap()
        .instance
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
            repository::ProcessPlanInput::Supplied(&plan),
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
        let command = stamp("late completion");
        let completion = runtime::plan_user_completion(
            &snapshot,
            &snapshot.user_tasks[0].user_task_id,
            &json!({"answer":"late"}),
            None,
            2_000,
            runtime::test_support::human_input(&snapshot, &snapshot.user_tasks[0].user_task_id, &command),
        None)
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
                    repository::ProcessPlanInput::Supplied(&first.2),
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
                    repository::ProcessPlanInput::Supplied(&second.2),
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
            &command,
            &started.instance_id,
            &snapshot.user_tasks[0].user_task_id,
            started.revision,
            &json!({"answer":"late"}),
            None,
            repository::ProcessPlanInput::Supplied(&completion),
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
            repeat: None,
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
            repository::ProcessPlanInput::Supplied(&plan),
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
            repository::ProcessPlanInput::Supplied(&plan2),
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
            repository::ProcessPlanInput::Supplied(&plan),
            3_000
        )
        .unwrap()
        .is_none());
        assert!(repository::fire_timer(
            &fixture.db,
            &second,
            &fixture.owner,
            Some(before.instance.revision),
            repository::ProcessPlanInput::Supplied(&plan2),
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
            embedded_model(
                service_model(&flow_id, ActivityVerification::Human),
                "Scope",
            ),
            "Scope",
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
            repository::ProcessPlanInput::Supplied(&plan),
            at_ms + 1_000,
        )
        .unwrap()
        .unwrap();
        assert!(outcome.cancelled_claims.is_empty());
        let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
            .unwrap();
        assert_eq!(after.jobs[0].status, "completed");
        assert!(after
            .scopes
            .iter()
            .filter(|scope| scope.parent_scope_id.is_some())
            .all(|scope| scope.status == ProcessInstanceStatus::Cancelled));
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
            &crate::processes::repository::ObservedActivityResult {
                result: (accepted_result).clone(),
                origin: crate::processes::repository::ActivityResultOrigin::Envelope,
                expression_observation: None,
            },
            after.instance.revision,
            repository::ProcessPlanInput::Supplied(&repository::RuntimePlan::initial(json!({}))),
            at_ms + 1_000,
        )
        .unwrap()
        .instance;
        assert_eq!(replay, after.instance);
    }

    #[tokio::test]
    async fn boundary_error_resolves_on_exact_activity_completion_without_erasing_unrelated_incident(
    ) {
        for (unrelated, scoped) in [(false, false), (true, false), (false, true), (true, true)] {
            let fixture = Fixture::new();
            let base = if scoped {
                embedded_model(user_model(None), "Scope")
            } else {
                user_model(None)
            };
            let activity = if scoped { "Scope" } else { "Work" };
            let end_id = base
                .nodes
                .iter()
                .find(|node| node.kind == ProcessNodeKind::End)
                .unwrap()
                .id
                .clone();
            let mut model = with_boundaries(base, activity, &[("Broken", true, 2)]);
            if unrelated {
                model = with_boundaries(model, activity, &[("Reminder", false, 1)]);
                model.nodes.push(ProcessNode {
                    id: "Choice".into(),
                    name: "Unmatched side choice".into(),
                    kind: ProcessNodeKind::ExclusiveGateway {
                        default_flow_id: None,
                    },
                    repeat: None,
                });
                model
                    .sequence_flows
                    .iter_mut()
                    .find(|edge| edge.source_id == "Reminder")
                    .unwrap()
                    .target_id = "Choice".into();
                for id in ["Choice_A", "Choice_B"] {
                    let mut branch = edge(id, "Choice", &end_id);
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
            if scoped {
                assert_eq!(
                    finished
                        .scopes
                        .iter()
                        .find(|scope| scope.parent_scope_id.is_some())
                        .unwrap()
                        .status,
                    ProcessInstanceStatus::Completed
                );
            }
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
                timer.token_id.clone().unwrap()
            );
            let reason = error.data["reason"].as_str().unwrap();
            assert!(reason.contains("controlled boundary storage failure"),
                "timer error reason: {reason}");
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
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap();
        let cancelled = repository::cancel_instance(
            &fixture.db,
            &fixture.owner,
            &stamp("cancel boundary error activation"),
            &started.instance_id,
            before.revision,
        )
        .unwrap()
        .instance;
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
            repository::ProcessPlanInput::Supplied(&plan),
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
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap();
        assert_eq!(after.status, ProcessInstanceStatus::Completed);
        assert_eq!(after.timers[0].status, ProcessTimerStatus::Fired);
    }

    #[tokio::test]
    async fn real_due_timer_completes_selected_inclusive_branch() {
        let fixture = Fixture::new();
        let mut model = catch_model(ProcessTimerSpec::Duration { seconds: 60 });
        model.nodes.extend([
            ProcessNode { id: "Split".into(), name: "Select waits".into(),
                kind: ProcessNodeKind::InclusiveGateway { default_flow_id: None },
                repeat: None, },
            ProcessNode { id: "Review".into(), name: "Independent review".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() },
                repeat: None, },
            ProcessNode { id: "Join".into(), name: "Selected waits done".into(),
                kind: ProcessNodeKind::InclusiveGateway { default_flow_id: None },
                repeat: None, },
        ]);
        model.sequence_flows = vec![
            edge("ToSplit", "Start_1", "Split"),
            edge("WaitBranch", "Split", "Wait"),
            edge("ReviewBranch", "Split", "Review"),
            edge("WaitJoin", "Wait", "Join"),
            edge("ReviewJoin", "Review", "Join"),
            edge("JoinEnd", "Join", "End_1"),
        ];
        model.sequence_flows[1].condition = Some("true".into());
        model.sequence_flows[2].condition = Some("true".into());
        let started = start_at(&fixture, &model, 1_000);
        let candidate = repository::due_timers(&fixture.db, 61_000, 32).unwrap().remove(0);
        assert_eq!(candidate.instance_id.as_deref(), Some(started.instance_id.as_str()));
        let drained = drain_due(&fixture.db, 61_000);
        drained.completion.unwrap();
        assert_eq!(drained.fired, 1);
        let partial = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(partial.receipts.len(), 1);
        assert_eq!(partial.receipts[0].branch_edge_id, "WaitBranch");
        let review = partial.user_tasks.iter().find(|task| task.node_id == "Review").unwrap();
        complete_work(&fixture, &started.instance_id, &review.user_task_id, 61_001);
        let final_state = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(final_state.instance.status, ProcessInstanceStatus::Completed);
        assert!(final_state.receipts.is_empty());
        let events = repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200).unwrap().0;
        assert_eq!(events.iter().filter(|event| event.kind == "inclusive_joined").count(), 1);
    }

    #[tokio::test]
    async fn boundary_after_real_gateway_join_enters_its_own_closed_region() {
        for inclusive in [false, true] {
        let fixture = Fixture::new();
        let mut model = with_boundaries(user_model(None), "Work", &[("Limit", true, 1)]);
        for id in ["Split", "Join", "SideSplit", "SideJoin"] {
            model.nodes.push(ProcessNode {
                id: id.into(),
                name: id.into(),
                kind: if inclusive { ProcessNodeKind::InclusiveGateway { default_flow_id: None } }
                    else { ProcessNodeKind::ParallelGateway },
                repeat: None,
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
                repeat: None,
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
        if inclusive {
            for flow in &mut model.sequence_flows {
                if ["M_A", "M_B", "S_A", "S_B"].contains(&flow.id.as_str()) {
                    flow.condition = Some("true".into());
                }
            }
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
                .filter(|event| event.kind == if inclusive { "inclusive_joined" } else { "parallel_joined" })
                .count(),
            2
        );
        }
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
                let command = stamp("approve immutable service result");
                let plan = runtime::plan_user_completion(
                    &snapshot,
                    &task.user_task_id,
                    &json!({}),
                    Some(true),
                    at_ms,
                    runtime::test_support::human_input(&snapshot, &task.user_task_id, &command),
                None)
                .unwrap();
                repository::complete_user_task(
                    &fixture.db,
                    &fixture.owner,
                    &command,
                    &started.instance_id,
                    &task.user_task_id,
                    snapshot.instance.revision,
                    &json!({}),
                    Some(true),
                    repository::ProcessPlanInput::Supplied(&plan),
                    at_ms,
                )
                .unwrap()
                .instance
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

    fn working_model(mut model: ProcessModel, zone: &str, start: u16, end: u16) -> ProcessModel {
        model.timer_timezone = Some(zone.into());
        model.work_calendar = Some(tentaflow_protocol::processes::ProcessWorkCalendar {
            name: "Private immutable working windows".into(),
            weekly_windows: (1..=7)
                .map(|weekday| tentaflow_protocol::processes::WorkWindow {
                    weekday,
                    start_minute: start,
                    end_minute: end,
                })
                .collect(),
            manual_days_off: Vec::new(),
            holiday_policy: tentaflow_protocol::processes::HolidayPolicy::None,
        });
        model
    }

    #[tokio::test]
    async fn working_future_v1_catch_and_boundary_activation_keep_exact_pin_after_v2_and_restart() {
        for boundary in [false, true] {
            let fixture = Fixture::new();
            let mut model = if boundary {
                let mut model = with_boundaries(user_model(None), "Work", &[("Limit", true, 3600)]);
                let ProcessNodeKind::BoundaryTimer { timer, .. } = &mut model
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == "Limit")
                    .unwrap()
                    .kind
                else {
                    panic!("boundary fixture");
                };
                *timer = ProcessTimerSpec::WorkingDuration { seconds: 3600 };
                model
            } else {
                catch_model(ProcessTimerSpec::WorkingDuration { seconds: 3600 })
            };
            let target = if boundary { "Work" } else { "Wait" };
            model.nodes.push(ProcessNode {
                id: "Gate".into(),
                name: "Future activation gate".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
                repeat: None,
            });
            model
                .sequence_flows
                .retain(|flow| flow.source_id != "Start_1");
            model.sequence_flows.extend([
                edge("ToGate", "Start_1", "Gate"),
                edge("GateNext", "Gate", target),
            ]);
            model = working_model(model, "America/Winnipeg", 540, 1020);
            let version = publish_model(&fixture, &model);
            let id = Uuid::new_v4().to_string();
            let anchor = at("2026-11-09T13:00:00Z");
            let command = stamp("V1 waits before future activation");
            let plan = runtime::plan_start(
                &version.model,
                &id,
                &fixture.owner,
                &version.definition_id,
                version.version,
                json!({}),
                StartCause::Manual,
                anchor,
                runtime::test_support::manual_input(&command),
            None)
            .unwrap();
            let waiting = repository::start_instance(
                &fixture.db,
                &fixture.owner,
                &command,
                &id,
                &version.definition_id,
                version.version,
                &json!({}),
                repository::ProcessPlanInput::Supplied(&plan),
                anchor,
            )
            .unwrap();
            assert!(waiting.timers.is_empty());
            let prior =
                repository::get_definition(&fixture.db, &fixture.owner, &version.definition_id)
                    .unwrap()
                    .0;
            let mut edited = prior.model.clone();
            for window in &mut edited.work_calendar.as_mut().unwrap().weekly_windows {
                window.start_minute = 720;
            }
            let saved = repository::save_definition(
                &fixture.db,
                &fixture.owner,
                &stamp("change V2 windows"),
                Some(&prior.definition_id),
                prior.draft_revision,
                "Changed current windows",
                "",
                &edited,
            )
            .unwrap();
            let (_, v2) = repository::publish_definition(
                &fixture.db,
                &fixture.owner,
                &stamp("publish new explicit calendar data"),
                &saved.definition_id,
                saved.draft_revision,
                &[],
                Some(true),
            )
            .unwrap();
            assert_ne!(v2.model.calendar_pin, version.model.calendar_pin);
            assert_eq!(
                super::super::calendar::working_due(
                    v2.model.calendar_pin.as_ref().unwrap(),
                    at("2026-11-09T14:00:00Z"),
                    3600
                )
                .unwrap(),
                at("2026-11-09T18:00:00Z")
            );
            let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &id).unwrap();
            let task = &snapshot.user_tasks[0];
            let activation = at("2026-11-09T14:00:00Z");
            let command = stamp("activate old pinned V1 after V2");
            let completion = runtime::plan_user_completion(
                &snapshot,
                &task.user_task_id,
                &json!({}),
                None,
                activation,
                runtime::test_support::human_input(&snapshot, &task.user_task_id, &command),
            None)
            .unwrap();
            let opened = repository::complete_user_task(
                &fixture.db,
                &fixture.owner,
                &command,
                &id,
                &task.user_task_id,
                snapshot.instance.revision,
                &json!({}),
                None,
                repository::ProcessPlanInput::Supplied(&completion),
                activation,
            )
            .unwrap()
            .instance;
            assert_eq!(opened.timers.len(), 1);
            let due = at("2026-11-09T15:00:00Z");
            assert_eq!(opened.timers[0].due_at_ms, Some(due));
            assert_eq!(
                opened.timers[0]
                    .working_time
                    .as_ref()
                    .unwrap()
                    .due_offset_seconds,
                Some(-18000)
            );
            let old = repository::runtime_snapshot(&fixture.db, &fixture.owner, &id).unwrap();
            assert_eq!(old.model.calendar_pin, version.model.calendar_pin);
            assert_eq!(old.timers[0].anchor_at_ms, activation);
            assert_eq!(old.timers[0].version, 1);
            if boundary {
                assert_ne!(old.timers[0].token_id, task.token_id);
            }
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
            assert_eq!(
                repository::get_instance(&reopened, &owner, &id, None)
                    .unwrap()
                    .timers,
                opened.timers
            );
            reopened
                .write()
                .unwrap()
                .execute(
                    "UPDATE user_accounts SET is_active=0 WHERE id=?1",
                    [&owner.user_id],
                )
                .unwrap();
            let blocked = drain_due(&reopened, due);
            blocked.completion.unwrap();
            assert_eq!(blocked.fired, 0);
            reopened
                .write()
                .unwrap()
                .execute(
                    "UPDATE user_accounts SET is_active=1 WHERE id=?1",
                    [&owner.user_id],
                )
                .unwrap();
            let fired = drain_due(&reopened, due + 60000);
            fired.completion.unwrap();
            assert_eq!(fired.fired, 1);
            let current = repository::get_instance(&reopened, &owner, &id, None).unwrap();
            assert_eq!(current.status, ProcessInstanceStatus::Completed);
            let events = repository::list_events(&reopened, &owner, &id, 0, 200)
                .unwrap()
                .0;
            let fired = events
                .iter()
                .find(|event| event.kind == "timer_fired")
                .unwrap();
            assert_eq!(fired.data["planned_due_at_ms"], due);
            assert_eq!(fired.data["fired_at_ms"], due + 60000);
            assert_eq!(
                fired.data["working_time"]["pin_sha256"],
                version.model.calendar_pin.as_ref().unwrap().sha256
            );
            assert_eq!(fired.data["working_time"]["due_offset_seconds"], -18000);
            assert_eq!(fired.data["timezone"], "America/Winnipeg");
            assert_eq!(
                repository::get_version(&reopened, &owner, &version.definition_id, 1).unwrap(),
                version
            );
            let replay = drain_due(&reopened, due + 60000);
            replay.completion.unwrap();
            assert_eq!(replay.fired, 0);
            drop(reopened);
            drop(directory);
        }
    }

    #[tokio::test]
    async fn working_start_snapshot_then_revoke_has_no_effect_and_restored_candidate_fires_once() {
        let fixture = Fixture::new();
        let mut model = super::super::model::starter_model();
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::WorkingDuration { seconds: 1 },
        };
        let model = working_model(model, "UTC", 0, 1440);
        let version = publish_model(&fixture, &model);
        let due = version.published_at_ms + 1000;
        let candidate = repository::due_timers(&fixture.db, due, 32)
            .unwrap()
            .remove(0);
        let snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
        let plan = plan_timer_fire(&snapshot, due, None).unwrap();
        fixture
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE user_accounts SET is_active=0 WHERE id=?1",
                [&fixture.owner.user_id],
            )
            .unwrap();
        let error =
            repository::fire_timer(&fixture.db, &candidate, &fixture.owner, None, repository::ProcessPlanInput::Supplied(&plan), due)
                .unwrap_err();
        assert!(error
            .downcast_ref::<repository::ProcessAuthorityDenied>()
            .is_some());
        assert!(repository::record_timer_blocked(
            &fixture.db,
            &candidate,
            &format!("{error:#}"),
            due
        )
        .unwrap());
        assert_eq!(
            fixture
                .db
                .read()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM bpmn_instances", [], |row| row
                    .get::<_, u32>(0))
                .unwrap(),
            0
        );
        fixture
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE user_accounts SET is_active=1 WHERE id=?1",
                [&fixture.owner.user_id],
            )
            .unwrap();
        let drained = drain_due(&fixture.db, due + 60000);
        drained.completion.unwrap();
        assert_eq!(drained.fired, 1);
        let repeated = drain_due(&fixture.db, due + 60000);
        repeated.completion.unwrap();
        assert_eq!(repeated.fired, 0);
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
            1
        );
    }

    #[tokio::test]
    async fn working_boundary_completion_and_fire_orders_cancel_only_exact_siblings() {
        for complete_first in [true, false] {
            let fixture = Fixture::new();
            let mut model = with_boundaries(
                user_model(None),
                "Work",
                &[("First", true, 1), ("Second", false, 2)],
            );
            for node in &mut model.nodes {
                if let ProcessNodeKind::BoundaryTimer { timer, .. } = &mut node.kind {
                    let ProcessTimerSpec::Duration { seconds } = timer else {
                        unreachable!()
                    };
                    *timer = ProcessTimerSpec::WorkingDuration { seconds: *seconds };
                }
            }
            let model = working_model(model, "UTC", 0, 1440);
            let anchor = at("2026-10-05T09:00:00Z");
            let started = start_at(&fixture, &model, anchor);
            let before =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            let task = &before.user_tasks[0];
            let command = stamp("completion wins working boundary");
            let completion = runtime::plan_user_completion(
                &before,
                &task.user_task_id,
                &json!({"answer":"completed"}),
                None,
                anchor + 1000,
                runtime::test_support::human_input(&before, &task.user_task_id, &command),
            None)
            .unwrap();
            let candidate = repository::due_timers(&fixture.db, anchor + 1000, 32)
                .unwrap()
                .remove(0);
            let snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
            let fire = plan_timer_fire(&snapshot, anchor + 1000, None).unwrap();
            if complete_first {
                repository::complete_user_task(
                    &fixture.db,
                    &fixture.owner,
                    &command,
                    &started.instance_id,
                    &task.user_task_id,
                    before.instance.revision,
                    &json!({"answer":"completed"}),
                    None,
                    repository::ProcessPlanInput::Supplied(&completion),
                    anchor + 1000,
                )
                .unwrap()
                .instance;
                assert!(repository::fire_timer(
                    &fixture.db,
                    &candidate,
                    &fixture.owner,
                    Some(before.instance.revision),
                    repository::ProcessPlanInput::Supplied(&fire),
                    anchor + 1000
                )
                .unwrap()
                .is_none());
            } else {
                let outcome = repository::fire_timer(
                    &fixture.db,
                    &candidate,
                    &fixture.owner,
                    Some(before.instance.revision),
                    repository::ProcessPlanInput::Supplied(&fire),
                    anchor + 1000,
                )
                .unwrap()
                .unwrap();
                assert!(outcome.cancelled_claims.is_empty());
                assert!(repository::complete_user_task(
                    &fixture.db,
                    &fixture.owner,
                    &command,
                    &started.instance_id,
                    &task.user_task_id,
                    before.instance.revision,
                    &json!({"answer":"completed"}),
                    None,
                    repository::ProcessPlanInput::Supplied(&completion),
                    anchor + 1000
                )
                .is_err());
            }
            let after =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            assert_eq!(after.instance.status, ProcessInstanceStatus::Completed);
            assert_eq!(
                after.user_tasks[0].status,
                if complete_first {
                    ProcessUserTaskStatus::Completed
                } else {
                    ProcessUserTaskStatus::Cancelled
                }
            );
            assert_eq!(
                after
                    .timers
                    .iter()
                    .filter(|timer| timer.status == ProcessTimerStatus::Fired)
                    .count(),
                usize::from(!complete_first)
            );
            assert!(after
                .timers
                .iter()
                .filter(|timer| timer.node_id == "Second")
                .all(|timer| timer.status == ProcessTimerStatus::Cancelled));
        }
    }

    #[tokio::test]
    async fn working_horizon_error_is_terminal_and_boundary_resolution_preserves_other_incidents() {
        let fixture = Fixture::new();
        let mut model = with_boundaries(
            user_model(None),
            "Work",
            &[("Broken", true, 1), ("Reminder", false, 1)],
        );
        let ProcessNodeKind::BoundaryTimer { timer, .. } = &mut model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Broken")
            .unwrap()
            .kind
        else {
            panic!("boundary");
        };
        *timer = ProcessTimerSpec::WorkingDuration { seconds: 31536000 };
        model = working_model(model, "UTC", 0, 1);
        model.nodes.push(ProcessNode {
            id: "Choice".into(),
            name: "Unmatched independent side path".into(),
            kind: ProcessNodeKind::ExclusiveGateway {
                default_flow_id: None,
            },
            repeat: None,
        });
        model
            .sequence_flows
            .iter_mut()
            .find(|edge| edge.source_id == "Reminder")
            .unwrap()
            .target_id = "Choice".into();
        for id in ["Never_A", "Never_B"] {
            let mut branch = edge(id, "Choice", "End_1");
            branch.condition = Some("false".into());
            model.sequence_flows.push(branch);
        }
        let anchor = at("2026-10-05T00:00:00Z");
        let started = start_at(&fixture, &model, anchor);
        assert_eq!(started.status, ProcessInstanceStatus::Incident);
        let failed = started
            .timers
            .iter()
            .find(|timer| timer.node_id == "Broken")
            .unwrap();
        assert_eq!(failed.status, ProcessTimerStatus::Error);
        assert_eq!(failed.due_at_ms, None);
        assert_eq!(
            failed.working_time.as_ref().unwrap().due_offset_seconds,
            None
        );
        let drained = drain_due(&fixture.db, anchor + 1000);
        drained.completion.unwrap();
        assert_eq!(drained.fired, 1);
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(snapshot.boundary_incidents.len(), 1);
        let linked = snapshot.boundary_incidents[0].incident_id.clone();
        let unrelated = snapshot
            .instance
            .incidents
            .iter()
            .find(|incident| incident.code == "NO_MATCHING_FLOW")
            .unwrap()
            .incident_id
            .clone();
        let task = snapshot
            .user_tasks
            .iter()
            .find(|task| task.node_id == "Work")
            .unwrap();
        let finished = complete_work(
            &fixture,
            &started.instance_id,
            &task.user_task_id,
            anchor + 1001,
        );
        assert_eq!(finished.status, ProcessInstanceStatus::Incident);
        assert!(finished
            .incidents
            .iter()
            .all(|incident| incident.incident_id != linked));
        assert_eq!(finished.incidents[0].incident_id, unrelated);
        assert!(repository::due_timers(&fixture.db, anchor + 2000, 32)
            .unwrap()
            .is_empty());
        let events =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        let errors = events
            .iter()
            .filter(|event| event.kind == "timer_error")
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].data["working_time"]["pin_sha256"],
            failed.working_time.as_ref().unwrap().pin_sha256
        );
        assert_eq!(errors[0].data["timezone"], "UTC");
        assert!(errors[0].data["reason"]
            .as_str()
            .unwrap()
            .contains("horizon"));
        let catch_model = working_model(
            catch_model(ProcessTimerSpec::WorkingDuration { seconds: 31536000 }),
            "UTC",
            0,
            1,
        );
        let version = publish_model(&fixture, &catch_model);
        let id = Uuid::new_v4().to_string();
        let variables = serde_json::to_value(&catch_model.variables).unwrap();
        let command = stamp("start due evaluation");
        let plan = runtime::plan_start(
            &version.model,
            &id,
            &fixture.owner,
            &version.definition_id,
            version.version,
            variables.clone(),
            StartCause::Manual,
            anchor,
            runtime::test_support::manual_input(&command),
        None)
        .unwrap();
        assert_eq!(plan.create_timers.len(), 1);
        let timer = &plan.create_timers[0];
        assert_eq!(timer.kind, ProcessTimerKind::Catch);
        assert_eq!(timer.status, ProcessTimerStatus::Error);
        assert_eq!(timer.due_at_ms, None);
        assert!(plan.create_tokens.iter().any(|token| {
            Some(token.token_id.as_str()) == timer.token_id.as_deref()
                && token.node_id == timer.node_id
                && token.status == "waiting"
        }));
        let invalid_timers: [(&str, fn(&mut ProcessTimer)); 11] = [
            ("revision", |timer| timer.revision = 2),
            ("occurrence", |timer| timer.occurrence = 0),
            ("error_due", |timer| {
                timer.due_at_ms = Some(timer.anchor_at_ms)
            }),
            ("pending_without_due", |timer| {
                timer.status = ProcessTimerStatus::Pending
            }),
            ("missing_reason", |timer| timer.last_reason = None),
            ("start_kind", |timer| timer.kind = ProcessTimerKind::Start),
            ("nonworking_catch", |timer| {
                timer.rule = ProcessTimerSpec::Duration { seconds: 1 }
            }),
            ("timezone", |timer| timer.timezone = "Europe/Warsaw".into()),
            ("organization", |timer| {
                timer.org_id = Uuid::new_v4().to_string()
            }),
            ("waiting_token", |timer| {
                timer.token_id = Some(Uuid::new_v4().to_string())
            }),
            ("instance", |timer| {
                timer.instance_id = Some(Uuid::new_v4().to_string())
            }),
        ];
        for (name, invalidate) in invalid_timers {
            let mut forged = plan.clone();
            invalidate(&mut forged.create_timers[0]);
            let command = stamp(&format!("invalid working catch {name}"));
            assert!(
                repository::start_instance(
                    &fixture.db,
                    &fixture.owner,
                    &command,
                    &id,
                    &version.definition_id,
                    version.version,
                    &variables,
                    repository::ProcessPlanInput::Supplied(&forged),
                    anchor,
                )
                .is_err(),
                "invalid timer {name} was accepted"
            );
            let remaining: i64 = fixture
                .db
                .read()
                .unwrap()
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM bpmn_instances WHERE instance_id=?1) + (SELECT COUNT(*) FROM bpmn_tokens WHERE instance_id=?1) + (SELECT COUNT(*) FROM bpmn_timers WHERE instance_id=?1) + (SELECT COUNT(*) FROM bpmn_incidents WHERE instance_id=?1) + (SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1) + (SELECT COUNT(*) FROM bpmn_commands WHERE command_id=?2)",
                    rusqlite::params![id, command.command_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(remaining, 0, "invalid timer {name} left committed rows");
        }
        let catch = repository::start_instance(
            &fixture.db,
            &fixture.owner,
            &command,
            &id,
            &version.definition_id,
            version.version,
            &variables,
            repository::ProcessPlanInput::Supplied(&plan),
            anchor,
        )
        .unwrap();
        assert_eq!(catch.status, ProcessInstanceStatus::Incident);
        assert_eq!(catch.timers[0].status, ProcessTimerStatus::Error);
        assert_eq!(catch.timers[0].due_at_ms, None);
        assert_eq!(catch.incidents.len(), 1);
        assert_eq!(catch.incidents[0].code, "TIMER_ERROR");
        let cancelled = repository::cancel_instance(
            &fixture.db,
            &fixture.owner,
            &stamp("cancel errored working catch"),
            &catch.instance_id,
            catch.revision,
        )
        .unwrap()
        .instance;
        assert_eq!(cancelled.status, ProcessInstanceStatus::Cancelled);
        let repeated = drain_due(&fixture.db, anchor + 2000);
        repeated.completion.unwrap();
        assert_eq!(repeated.fired, 0);
    }
}
