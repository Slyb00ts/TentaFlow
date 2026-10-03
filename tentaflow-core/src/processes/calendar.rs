// ============ File: calendar.rs — verified private calendar pins and real UTC working-time arithmetic ============

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use anyhow::{bail, ensure, Context, Result};
use chrono::{Datelike, Days, NaiveDate};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tentaflow_protocol::processes::{
    HolidayPolicy, ProcessCalendarPin, ProcessCalendarPinState, ProcessHolidayRuleKind,
    ProcessLegalRelease, ProcessModel, ProcessOffsetTransition, ProcessTimerSpec,
    ProcessTimezoneData, ProcessWorkCalendar, ProcessWorkingTimeSummary, WorkWindow,
};

mod data {
    include!(concat!(env!("OUT_DIR"), "/process_calendar_data.rs"));
}

const MAX_CALENDAR_BYTES: usize = 128 * 1024;
const MAX_DATES: usize = 6500;
const MAX_INTERSECTIONS: usize = 52000;
const PIN_DOMAIN: &[u8] = b"tentaflow.process.calendar.pin\0";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ZoneOffsets {
    initial_offset_seconds: i32,
    transitions: Vec<ProcessOffsetTransition>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimezoneDataset {
    tzdb_release: String,
    valid_from_utc_ms: i64,
    valid_until_utc_ms: i64,
    zones: BTreeMap<String, ZoneOffsets>,
}

struct TrustedData {
    legal: ProcessLegalRelease,
    timezone: TimezoneDataset,
    manifest: serde_json::Value,
}

fn trusted_data() -> Result<&'static TrustedData> {
    static TRUSTED: OnceLock<std::result::Result<TrustedData, String>> = OnceLock::new();
    let result = TRUSTED.get_or_init(|| {
        let load = || -> Result<TrustedData> {
            let legal: ProcessLegalRelease = serde_json::from_slice(data::LEGAL_RELEASE_JSON)?;
            validate_legal(&legal)?;
            ensure!(
                legal.audit_manifest_sha256 == digest(data::LEGAL_AUDIT_JSON),
                "embedded legal audit manifest checksum mismatch"
            );
            let timezone: TimezoneDataset = serde_json::from_slice(data::TZDB_JSON)?;
            let manifest: serde_json::Value = serde_json::from_slice(data::TZDB_MANIFEST_JSON)?;
            ensure!(
                manifest["dataset_sha256"].as_str() == Some(digest(data::TZDB_JSON).as_str())
                    && manifest["release_id"].as_str() == Some(timezone.tzdb_release.as_str())
                    && manifest["horizon_start_ms"].as_i64() == Some(timezone.valid_from_utc_ms)
                    && manifest["horizon_end_ms"].as_i64() == Some(timezone.valid_until_utc_ms)
                    && manifest["zone_count"].as_u64() == Some(timezone.zones.len() as u64),
                "embedded complete timezone dataset identity mismatch"
            );
            Ok(TrustedData {
                legal,
                timezone,
                manifest,
            })
        };
        load().map_err(|error| format!("{error:#}"))
    });
    match result {
        Ok(data) => Ok(data),
        Err(error) => bail!("trusted calendar data is unavailable: {error}"),
    }
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hash_text(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "calendar SHA-256 must be 64 lowercase hex characters"
    );
    Ok(())
}

fn bounded_text(value: &str, max_bytes: usize, label: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.len() <= max_bytes
            && !value.chars().any(char::is_control),
        "{label} is empty, contains controls or exceeds {max_bytes} UTF-8 bytes"
    );
    Ok(())
}

fn date(value: &str) -> Result<NaiveDate> {
    ensure!(
        value.len() == 10 && value.is_ascii(),
        "calendar date requires YYYY-MM-DD"
    );
    let parsed = NaiveDate::parse_from_str(value, "%Y-%m-%d").context("invalid calendar date")?;
    ensure!(
        parsed.format("%Y-%m-%d").to_string() == value,
        "noncanonical calendar date"
    );
    Ok(parsed)
}

pub fn validate_work_calendar(calendar: &ProcessWorkCalendar) -> Result<()> {
    bounded_text(&calendar.name, 256, "calendar name")?;
    ensure!(
        (1..=56).contains(&calendar.weekly_windows.len()),
        "calendar requires 1..56 weekly windows"
    );
    let mut previous: Option<&WorkWindow> = None;
    let mut per_day = [0u8; 7];
    for window in &calendar.weekly_windows {
        ensure!(
            (1..=7).contains(&window.weekday)
                && window.start_minute < window.end_minute
                && window.end_minute <= 1440,
            "calendar window requires weekday 1..7 and 0 <= start < end <= 1440"
        );
        let count = &mut per_day[usize::from(window.weekday - 1)];
        *count += 1;
        ensure!(
            *count <= 8,
            "calendar supports at most 8 windows per weekday"
        );
        if let Some(previous) = previous {
            ensure!(
                previous.weekday < window.weekday
                    || (previous.weekday == window.weekday
                        && previous.end_minute <= window.start_minute),
                "calendar windows must be sorted, unique and nonoverlapping"
            );
        }
        previous = Some(window);
    }
    ensure!(
        calendar.manual_days_off.len() <= 256,
        "calendar supports at most 256 manual closures"
    );
    let mut previous = None;
    for closure in &calendar.manual_days_off {
        let parsed = date(&closure.date)?;
        ensure!(
            closure.date.as_str() >= "2024-01-01" && closure.date.as_str() < "2041-01-01",
            "manual closure is outside calendar coverage"
        );
        ensure!(
            previous.is_none_or(|previous| previous < parsed),
            "manual closures must be sorted and unique"
        );
        bounded_text(&closure.reason, 128, "manual closure reason")?;
        previous = Some(parsed);
    }
    ensure!(
        serde_json::to_vec(calendar)?.len() <= MAX_CALENDAR_BYTES,
        "calendar configuration exceeds 128 KiB"
    );
    Ok(())
}

fn validate_legal(release: &ProcessLegalRelease) -> Result<()> {
    bounded_text(&release.release_id, 128, "legal release ID")?;
    date(&release.as_of_date)?;
    let start = date(&release.valid_from)?;
    let end = date(&release.valid_until)?;
    ensure!(start < end, "invalid legal coverage");
    hash_text(&release.audit_manifest_sha256)?;
    ensure!(
        (1..=8).contains(&release.sources.len()),
        "legal release requires 1..8 primary sources"
    );
    let mut previous = None;
    for source in &release.sources {
        bounded_text(&source.source_id, 128, "legal source ID")?;
        bounded_text(&source.url, 1024, "legal source URL")?;
        hash_text(&source.sha256)?;
        date(&source.retrieved_on)?;
        ensure!(
            previous.is_none_or(|previous: &str| previous < source.source_id.as_str()),
            "legal sources must be ordered and unique"
        );
        previous = Some(source.source_id.as_str());
    }
    ensure!(
        (1..=64).contains(&release.rules.len()),
        "legal release requires 1..64 rules"
    );
    let mut previous = None;
    for rule in &release.rules {
        bounded_text(&rule.rule_id, 128, "holiday rule ID")?;
        ensure!(
            previous.is_none_or(|previous: &str| previous < rule.rule_id.as_str()),
            "holiday rules must be ordered and unique"
        );
        previous = Some(rule.rule_id.as_str());
        ensure!(
            release
                .sources
                .iter()
                .any(|source| source.source_id == rule.source_id),
            "holiday rule has an unknown legal source"
        );
        let effective = date(&rule.effective_from)?;
        if let Some(until) = &rule.effective_until {
            ensure!(
                effective < date(until)?,
                "invalid holiday rule effective interval"
            );
        }
        match rule.kind {
            ProcessHolidayRuleKind::Fixed { month, day } => {
                NaiveDate::from_ymd_opt(2024, u32::from(month), u32::from(day))
                    .context("invalid fixed holiday date")?;
            }
            ProcessHolidayRuleKind::GregorianEasterOffset { days } => ensure!(
                (-366..=366).contains(&days),
                "Easter offset is out of bounds"
            ),
            ProcessHolidayRuleKind::Weekday { weekday } => {
                ensure!((1..=7).contains(&weekday), "invalid holiday weekday")
            }
        }
    }
    Ok(())
}

fn validate_timezone(zone: &ProcessTimezoneData) -> Result<()> {
    bounded_text(&zone.iana_name, 128, "calendar IANA name")?;
    bounded_text(&zone.release_id, 128, "timezone release ID")?;
    bounded_text(&zone.source_url, 1024, "timezone source URL")?;
    hash_text(&zone.source_sha256)?;
    hash_text(&zone.dataset_sha256)?;
    ensure!(
        zone.horizon_start_ms < zone.horizon_end_ms && zone.transitions.len() <= 128,
        "timezone horizon/transition count is invalid"
    );
    ensure!(
        zone.initial_offset_seconds > -86400 && zone.initial_offset_seconds < 86400,
        "invalid initial UTC offset"
    );
    let mut previous_at = zone.horizon_start_ms;
    let mut previous_offset = zone.initial_offset_seconds;
    for transition in &zone.transitions {
        ensure!(
            previous_at < transition.at_utc_ms && transition.at_utc_ms < zone.horizon_end_ms,
            "timezone transitions must be ordered inside coverage"
        );
        ensure!(
            transition.offset_seconds > -86400
                && transition.offset_seconds < 86400
                && transition.offset_seconds != previous_offset,
            "invalid or redundant timezone offset"
        );
        previous_at = transition.at_utc_ms;
        previous_offset = transition.offset_seconds;
    }
    Ok(())
}

fn trusted_timezone(data: &TrustedData, name: &str) -> Result<ProcessTimezoneData> {
    let zone = data
        .timezone
        .zones
        .get(name)
        .context("calendar IANA name is not in the complete retained timezone dataset")?;
    let field = |name: &str| -> Result<String> {
        Ok(data.manifest[name]
            .as_str()
            .context("embedded timezone manifest field missing")?
            .to_string())
    };
    Ok(ProcessTimezoneData {
        iana_name: name.to_string(),
        release_id: data.timezone.tzdb_release.clone(),
        horizon_start_ms: data.timezone.valid_from_utc_ms,
        horizon_end_ms: data.timezone.valid_until_utc_ms,
        initial_offset_seconds: zone.initial_offset_seconds,
        transitions: zone.transitions.clone(),
        source_url: field("source_url")?,
        source_sha256: field("source_sha256")?,
        dataset_sha256: field("dataset_sha256")?,
    })
}

fn pin_digest(pin: &ProcessCalendarPin) -> Result<String> {
    #[derive(Serialize)]
    struct Payload<'a> {
        calendar: &'a ProcessWorkCalendar,
        legal_release: &'a ProcessLegalRelease,
        timezone_data: &'a ProcessTimezoneData,
    }
    let mut hash = Sha256::new();
    hash.update(PIN_DOMAIN);
    hash.update(serde_json::to_vec(&Payload {
        calendar: &pin.calendar,
        legal_release: &pin.legal_release,
        timezone_data: &pin.timezone_data,
    })?);
    Ok(hex::encode(hash.finalize()))
}

fn calendar_bytes(
    calendar: Option<&ProcessWorkCalendar>,
    pin: Option<&ProcessCalendarPin>,
) -> Result<()> {
    #[derive(Serialize)]
    struct Fields<'a> {
        #[serde(skip_serializing_if = "Option::is_none")]
        work_calendar: Option<&'a ProcessWorkCalendar>,
        #[serde(skip_serializing_if = "Option::is_none")]
        calendar_pin: Option<&'a ProcessCalendarPin>,
    }
    ensure!(
        serde_json::to_vec(&Fields {
            work_calendar: calendar,
            calendar_pin: pin
        })?
        .len()
            <= MAX_CALENDAR_BYTES,
        "calendar and complete pin exceed 128 KiB"
    );
    Ok(())
}

pub fn verify_calendar_pin(pin: &ProcessCalendarPin) -> Result<()> {
    validate_work_calendar(&pin.calendar)?;
    validate_legal(&pin.legal_release)?;
    validate_timezone(&pin.timezone_data)?;
    calendar_bytes(Some(&pin.calendar), Some(pin))?;
    hash_text(&pin.sha256)?;
    ensure!(
        pin.sha256 == pin_digest(pin)?,
        "calendar pin digest mismatch"
    );
    let trusted = trusted_data()?;
    ensure!(
        pin.legal_release == trusted.legal,
        "calendar pin legal release is unknown or differs from retained reviewed data"
    );
    ensure!(
        pin.timezone_data == trusted_timezone(trusted, &pin.timezone_data.iana_name)?,
        "calendar pin timezone release, provenance or complete offsets differ from retained data"
    );
    Ok(())
}

pub fn calendar_pin_state(model: &ProcessModel) -> Result<Option<ProcessCalendarPinState>> {
    if let Some(pin) = &model.calendar_pin {
        verify_calendar_pin(pin)?;
    }
    calendar_bytes(model.work_calendar.as_ref(), model.calendar_pin.as_ref())?;
    let Some(calendar) = &model.work_calendar else {
        ensure!(
            model.calendar_pin.is_none(),
            "calendar pin requires a configured private calendar"
        );
        return Ok(None);
    };
    validate_work_calendar(calendar)?;
    let zone = model
        .timer_timezone
        .as_deref()
        .context("configured calendar requires an explicit IANA timezone")?;
    trusted_timezone(trusted_data()?, zone)?;
    Ok(Some(match &model.calendar_pin {
        None => ProcessCalendarPinState::Unpinned,
        Some(pin) if &pin.calendar == calendar && pin.timezone_data.iana_name == zone => {
            ProcessCalendarPinState::Current
        }
        Some(_) => ProcessCalendarPinState::Stale,
    }))
}

pub fn mint_calendar_pin(
    calendar: &ProcessWorkCalendar,
    iana_name: &str,
) -> Result<ProcessCalendarPin> {
    validate_work_calendar(calendar)?;
    let trusted = trusted_data()?;
    let mut pin = ProcessCalendarPin {
        calendar: calendar.clone(),
        legal_release: trusted.legal.clone(),
        timezone_data: trusted_timezone(trusted, iana_name)?,
        sha256: String::new(),
    };
    pin.sha256 = pin_digest(&pin)?;
    verify_calendar_pin(&pin)?;
    Ok(pin)
}

pub(super) fn pinned_offset_at(pin: &ProcessCalendarPin, at_ms: i64) -> Result<i32> {
    verify_calendar_pin(pin)?;
    let zone = &pin.timezone_data;
    ensure!(
        at_ms >= zone.horizon_start_ms && at_ms < zone.horizon_end_ms,
        "instant is outside pinned timezone coverage"
    );
    let index = zone
        .transitions
        .partition_point(|transition| transition.at_utc_ms <= at_ms);
    Ok(if index == 0 {
        zone.initial_offset_seconds
    } else {
        zone.transitions[index - 1].offset_seconds
    })
}

pub(super) fn working_time_summary(
    rule: &ProcessTimerSpec,
    pin: Option<&ProcessCalendarPin>,
    due_at_ms: Option<i64>,
) -> Result<Option<ProcessWorkingTimeSummary>> {
    if !matches!(rule, ProcessTimerSpec::WorkingDuration { .. }) {
        return Ok(None);
    }
    let pin = pin.context("working timer has no immutable calendar pin")?;
    verify_calendar_pin(pin)?;
    Ok(Some(ProcessWorkingTimeSummary {
        calendar_name: pin.calendar.name.clone(),
        holiday_policy: pin.calendar.holiday_policy.clone(),
        pin_sha256: pin.sha256.clone(),
        legal_release_id: pin.legal_release.release_id.clone(),
        legal_as_of_date: pin.legal_release.as_of_date.clone(),
        tzdb_release_id: pin.timezone_data.release_id.clone(),
        due_offset_seconds: due_at_ms.map(|at| pinned_offset_at(pin, at)).transpose()?,
    }))
}

fn gregorian_easter(year: i32) -> Result<NaiveDate> {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let value = h + l - 7 * m + 114;
    NaiveDate::from_ymd_opt(year, (value / 31) as u32, (value % 31 + 1) as u32)
        .context("Gregorian Easter is outside supported dates")
}

fn statutory_closed(release: &ProcessLegalRelease, day: NaiveDate) -> Result<bool> {
    for rule in &release.rules {
        if day < date(&rule.effective_from)?
            || rule
                .effective_until
                .as_ref()
                .map(|until| date(until))
                .transpose()?
                .is_some_and(|until| day >= until)
        {
            continue;
        }
        let closed = match rule.kind {
            ProcessHolidayRuleKind::Fixed {
                month,
                day: holiday_day,
            } => day.month() == u32::from(month) && day.day() == u32::from(holiday_day),
            ProcessHolidayRuleKind::Weekday { weekday } => {
                day.weekday().number_from_monday() == u32::from(weekday)
            }
            ProcessHolidayRuleKind::GregorianEasterOffset { days } => {
                gregorian_easter(day.year())?
                    .checked_add_signed(chrono::Duration::days(i64::from(days)))
                    .context("holiday arithmetic overflow")?
                    == day
            }
        };
        if closed {
            return Ok(true);
        }
    }
    Ok(false)
}

fn local_date(at_ms: i64, offset_seconds: i32) -> Result<NaiveDate> {
    let local = at_ms
        .checked_add(i64::from(offset_seconds) * 1000)
        .context("calendar offset arithmetic overflow")?;
    Ok(chrono::DateTime::from_timestamp_millis(local)
        .context("calendar date horizon exceeded")?
        .date_naive())
}

fn window_intersection(
    start: i64,
    end: i64,
    offset_seconds: i32,
    day: NaiveDate,
    window: &WorkWindow,
) -> Result<Option<(i64, i64)>> {
    let midnight = day
        .and_hms_opt(0, 0, 0)
        .context("invalid local calendar midnight")?
        .and_utc()
        .timestamp_millis();
    let offset = i64::from(offset_seconds) * 1000;
    let opening = midnight
        .checked_add(i64::from(window.start_minute) * 60000)
        .and_then(|at| at.checked_sub(offset))
        .context("calendar opening overflow")?;
    let closing = midnight
        .checked_add(i64::from(window.end_minute) * 60000)
        .and_then(|at| at.checked_sub(offset))
        .context("calendar closing overflow")?;
    let from = start.max(opening);
    let until = end.min(closing);
    Ok((from < until).then_some((from, until)))
}

pub fn working_due(pin: &ProcessCalendarPin, anchor_at_ms: i64, seconds: u32) -> Result<i64> {
    verify_calendar_pin(pin)?;
    ensure!(
        (1..=31_536_000).contains(&seconds),
        "WorkingDuration seconds must be 1..31536000"
    );
    let coverage_start = date(&pin.legal_release.valid_from)?;
    let coverage_end = date(&pin.legal_release.valid_until)?;
    let anchor_date = local_date(anchor_at_ms, pinned_offset_at(pin, anchor_at_ms)?)?;
    ensure!(
        coverage_start <= anchor_date && anchor_date < coverage_end,
        "working timer anchor is outside pinned legal coverage"
    );
    let closures = pin
        .calendar
        .manual_days_off
        .iter()
        .map(|closure| date(&closure.date))
        .collect::<Result<HashSet<_>>>()?;
    let mut remaining = i64::from(seconds)
        .checked_mul(1000)
        .context("working duration arithmetic overflow")?;
    let mut visited_dates = 0;
    let mut intersections = 0;
    let mut segment_start = pin.timezone_data.horizon_start_ms;
    let mut offset = pin.timezone_data.initial_offset_seconds;
    for index in 0..=pin.timezone_data.transitions.len() {
        let segment_end = pin
            .timezone_data
            .transitions
            .get(index)
            .map_or(pin.timezone_data.horizon_end_ms, |transition| {
                transition.at_utc_ms
            });
        let start = segment_start.max(anchor_at_ms);
        if start < segment_end {
            let mut day = local_date(start, offset)?;
            let last_day = local_date(segment_end - 1, offset)?;
            while day <= last_day && day < coverage_end {
                visited_dates += 1;
                ensure!(
                    visited_dates <= MAX_DATES,
                    "working calendar date-visit budget exceeded"
                );
                if day >= coverage_start
                    && !closures.contains(&day)
                    && (pin.calendar.holiday_policy == HolidayPolicy::None
                        || !statutory_closed(&pin.legal_release, day)?)
                {
                    for window in pin.calendar.weekly_windows.iter().filter(|window| {
                        u32::from(window.weekday) == day.weekday().number_from_monday()
                    }) {
                        intersections += 1;
                        ensure!(
                            intersections <= MAX_INTERSECTIONS,
                            "working calendar intersection budget exceeded"
                        );
                        if let Some((from, until)) =
                            window_intersection(start, segment_end, offset, day, window)?
                        {
                            let available = until
                                .checked_sub(from)
                                .context("working interval overflow")?;
                            if remaining <= available {
                                return from.checked_add(remaining).context("working due overflow");
                            }
                            remaining -= available;
                        }
                    }
                }
                day = day
                    .checked_add_days(Days::new(1))
                    .context("working calendar date overflow")?;
            }
        }
        segment_start = segment_end;
        if let Some(transition) = pin.timezone_data.transitions.get(index) {
            offset = transition.offset_seconds;
        }
    }
    bail!("working duration exhausts the pinned legal/timezone horizon")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Offset, TimeZone};
    use tentaflow_protocol::processes::ManualDayOff;

    fn at(value: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp_millis()
    }

    fn calendar(weekday: u8, start_minute: u16, end_minute: u16) -> ProcessWorkCalendar {
        ProcessWorkCalendar {
            name: "Private working calendar".into(),
            weekly_windows: vec![WorkWindow {
                weekday,
                start_minute,
                end_minute,
            }],
            manual_days_off: Vec::new(),
            holiday_policy: HolidayPolicy::None,
        }
    }

    #[test]
    fn complete_retained_tables_cover_all_names_and_pin_winnipeg_without_host_offsets() {
        let trusted = trusted_data().unwrap();
        assert_eq!(trusted.timezone.zones.len(), 597);
        for name in trusted.timezone.zones.keys() {
            assert!(name.parse::<chrono_tz::Tz>().is_ok(), "{name}");
            let zone = trusted_timezone(trusted, name).unwrap();
            validate_timezone(&zone).unwrap();
        }
        assert_eq!(
            trusted
                .timezone
                .zones
                .values()
                .map(|zone| zone.transitions.len())
                .max(),
            Some(36)
        );
        assert_eq!(
            trusted.timezone.valid_until_utc_ms,
            at("2041-01-02T00:00:00Z")
        );
        let pin = mint_calendar_pin(&calendar(1, 540, 1020), "America/Winnipeg").unwrap();
        let due = working_due(&pin, at("2026-11-09T14:00:00Z"), 3600).unwrap();
        assert_eq!(due, at("2026-11-09T15:00:00Z"));
        assert_eq!(pinned_offset_at(&pin, due).unwrap(), -18000);
        let old_zone: chrono_tz::Tz = "America/Winnipeg".parse().unwrap();
        let utc = chrono::DateTime::from_timestamp_millis(due).unwrap();
        assert_eq!(
            old_zone
                .offset_from_utc_datetime(&utc.naive_utc())
                .fix()
                .local_minus_utc(),
            -21600
        );
        let summary = working_time_summary(
            &ProcessTimerSpec::WorkingDuration { seconds: 3600 },
            Some(&pin),
            Some(due),
        )
        .unwrap()
        .unwrap();
        assert_eq!(summary.due_offset_seconds, Some(-18000));
        assert_eq!(summary.tzdb_release_id, "2026e");
        assert!(trusted_timezone(trusted, "Mars/Unknown").is_err());
        assert!(trusted_timezone(trusted, " Europe/Warsaw").is_err());
    }

    #[test]
    fn canonical_digest_is_deterministic_and_full_retained_provenance_cannot_be_forged() {
        let pin = mint_calendar_pin(&calendar(1, 540, 1020), "Europe/Warsaw").unwrap();
        assert_eq!(
            pin.sha256,
            "72d3b30270ba4619d6dbc098b488e52242d68fa13feb44f70be7f5d3b1d54f65"
        );
        assert_eq!(
            pin,
            mint_calendar_pin(&pin.calendar, "Europe/Warsaw").unwrap()
        );
        let mut mutants = Vec::new();
        let mut altered = pin.clone();
        altered.timezone_data.transitions.pop();
        mutants.push(altered);
        let mut altered = pin.clone();
        altered.timezone_data.initial_offset_seconds += 1;
        mutants.push(altered);
        let mut altered = pin.clone();
        altered.timezone_data.horizon_end_ms -= 1000;
        mutants.push(altered);
        let mut altered = pin.clone();
        altered.timezone_data.source_url.push_str("?forged");
        mutants.push(altered);
        let mut altered = pin.clone();
        altered.timezone_data.source_sha256 = "0".repeat(64);
        mutants.push(altered);
        let mut altered = pin.clone();
        altered.timezone_data.release_id = "2027a".into();
        mutants.push(altered);
        let mut altered = pin.clone();
        altered.legal_release.rules.pop();
        mutants.push(altered);
        let mut altered = pin.clone();
        altered.legal_release.as_of_date = "2026-10-03".into();
        mutants.push(altered);
        let mut altered = pin.clone();
        altered.legal_release.sources[0].sha256 = "0".repeat(64);
        mutants.push(altered);
        for mut altered in mutants {
            altered.sha256 = pin_digest(&altered).unwrap();
            assert!(verify_calendar_pin(&altered).is_err());
        }
        let mut changed_digest = pin.clone();
        changed_digest.sha256 = "A".repeat(64);
        assert!(verify_calendar_pin(&changed_digest).is_err());
        assert_eq!(pin.legal_release.sources.len(), 3);
        assert_eq!(pin.legal_release.rules.len(), 15);
        assert_eq!(
            pin.legal_release.audit_manifest_sha256,
            digest(data::LEGAL_AUDIT_JSON)
        );
    }

    #[test]
    fn trusted_pin_currency_tracks_only_configured_calendar_and_explicit_zone() {
        let mut model = super::super::model::starter_model();
        assert_eq!(calendar_pin_state(&model).unwrap(), None);
        model.work_calendar = Some(calendar(1, 540, 1020));
        assert!(calendar_pin_state(&model).is_err());
        model.timer_timezone = Some("Europe/Warsaw".into());
        assert_eq!(
            calendar_pin_state(&model).unwrap(),
            Some(ProcessCalendarPinState::Unpinned)
        );
        model.calendar_pin = Some(
            mint_calendar_pin(model.work_calendar.as_ref().unwrap(), "Europe/Warsaw").unwrap(),
        );
        assert_eq!(
            calendar_pin_state(&model).unwrap(),
            Some(ProcessCalendarPinState::Current)
        );
        let retained = model.calendar_pin.clone();
        model.work_calendar.as_mut().unwrap().holiday_policy = HolidayPolicy::PolandStatutory;
        assert_eq!(
            calendar_pin_state(&model).unwrap(),
            Some(ProcessCalendarPinState::Stale)
        );
        assert_eq!(model.calendar_pin, retained);
        model.work_calendar = Some(model.calendar_pin.as_ref().unwrap().calendar.clone());
        model.timer_timezone = Some("America/Winnipeg".into());
        assert_eq!(
            calendar_pin_state(&model).unwrap(),
            Some(ProcessCalendarPinState::Stale)
        );
        model.work_calendar = None;
        assert!(calendar_pin_state(&model).is_err());
    }

    #[test]
    fn weekly_lunch_closed_anchors_milliseconds_and_closing_endpoints_use_real_work() {
        let mut config = calendar(1, 540, 1020);
        config.weekly_windows = (1..=5)
            .map(|weekday| WorkWindow {
                weekday,
                start_minute: 540,
                end_minute: 1020,
            })
            .collect();
        let pin = mint_calendar_pin(&config, "Europe/Warsaw").unwrap();
        let friday = at("2026-06-19T13:00:00Z");
        assert_eq!(
            working_due(&pin, friday, 86400).unwrap(),
            at("2026-06-24T13:00:00Z")
        );
        assert_ne!(working_due(&pin, friday, 86400).unwrap(), friday + 86400000);
        let mut lunch = calendar(1, 540, 720);
        lunch.weekly_windows.push(WorkWindow {
            weekday: 1,
            start_minute: 780,
            end_minute: 1020,
        });
        let lunch = mint_calendar_pin(&lunch, "Europe/Warsaw").unwrap();
        assert_eq!(
            working_due(&lunch, at("2026-06-22T09:59:59.500Z"), 1).unwrap(),
            at("2026-06-22T11:00:00.500Z")
        );
        assert_eq!(
            working_due(&lunch, at("2026-06-22T09:00:00Z"), 3600).unwrap(),
            at("2026-06-22T10:00:00Z")
        );
        assert_eq!(
            working_due(&lunch, at("2026-06-22T10:30:00Z"), 60).unwrap(),
            at("2026-06-22T11:01:00Z")
        );
    }

    #[test]
    fn reviewed_statutory_effective_dates_easter_sunday_and_manual_compensation_apply() {
        let release = &trusted_data().unwrap().legal;
        assert!(!statutory_closed(release, date("2024-12-24").unwrap()).unwrap());
        assert!(statutory_closed(release, date("2025-12-24").unwrap()).unwrap());
        for closed in [
            "2026-04-05",
            "2026-04-06",
            "2026-05-24",
            "2026-06-04",
            "2026-06-21",
        ] {
            assert!(
                statutory_closed(release, date(closed).unwrap()).unwrap(),
                "{closed}"
            );
        }
        assert!(!statutory_closed(release, date("2026-06-22").unwrap()).unwrap());
        let mut config = calendar(1, 540, 1020);
        config.holiday_policy = HolidayPolicy::PolandStatutory;
        config.weekly_windows = (1..=7)
            .map(|weekday| WorkWindow {
                weekday,
                start_minute: 540,
                end_minute: 1020,
            })
            .collect();
        config.manual_days_off.push(ManualDayOff {
            date: "2026-08-17".into(),
            reason: "Compensatory day chosen by the owner".into(),
        });
        let pin = mint_calendar_pin(&config, "Europe/Warsaw").unwrap();
        assert_eq!(
            working_due(&pin, at("2026-08-15T07:00:00Z"), 3600).unwrap(),
            at("2026-08-18T08:00:00Z")
        );
        config.manual_days_off.clear();
        let pin = mint_calendar_pin(&config, "Europe/Warsaw").unwrap();
        assert_eq!(
            working_due(&pin, at("2026-08-15T07:00:00Z"), 3600).unwrap(),
            at("2026-08-17T08:00:00Z")
        );
        config.holiday_policy = HolidayPolicy::None;
        let pin = mint_calendar_pin(&config, "Europe/Warsaw").unwrap();
        assert_eq!(
            working_due(&pin, at("2026-08-16T07:00:00Z"), 3600).unwrap(),
            at("2026-08-16T08:00:00Z")
        );
    }

    #[test]
    fn partial_hebron_fold_preserves_closed_gap_and_saturday_gap_has_no_preimage() {
        let fold = mint_calendar_pin(&calendar(6, 75, 90), "Asia/Hebron").unwrap();
        assert_eq!(
            working_due(&fold, at("2026-10-23T22:15:00Z"), 1200).unwrap(),
            at("2026-10-23T23:20:00Z")
        );
        let gap = mint_calendar_pin(&calendar(6, 120, 180), "Asia/Hebron").unwrap();
        assert_eq!(
            working_due(&gap, at("2026-03-28T00:00:00Z"), 60).unwrap(),
            at("2026-04-03T23:01:00Z")
        );
    }

    #[test]
    fn warsaw_and_lord_howe_gap_fold_count_utc_seconds_and_policy_none_is_explicit() {
        let warsaw = mint_calendar_pin(&calendar(7, 60, 240), "Europe/Warsaw").unwrap();
        assert_eq!(
            working_due(&warsaw, at("2026-03-29T00:00:00Z"), 7200).unwrap(),
            at("2026-03-29T02:00:00Z")
        );
        let fold = mint_calendar_pin(&calendar(7, 135, 150), "Europe/Warsaw").unwrap();
        assert_eq!(
            working_due(&fold, at("2026-10-25T00:15:00Z"), 1200).unwrap(),
            at("2026-10-25T01:20:00Z")
        );
        let howe = mint_calendar_pin(&calendar(7, 105, 135), "Australia/Lord_Howe").unwrap();
        assert_eq!(
            working_due(&howe, at("2026-04-04T14:45:00Z"), 2700).unwrap(),
            at("2026-04-04T15:45:00Z")
        );
        assert_eq!(
            working_due(&howe, at("2026-10-03T15:15:00Z"), 1800).unwrap(),
            at("2026-10-10T15:00:00Z")
        );
        let mut statutory = warsaw.calendar.clone();
        statutory.holiday_policy = HolidayPolicy::PolandStatutory;
        let statutory = mint_calendar_pin(&statutory, "Europe/Warsaw").unwrap();
        assert!(working_due(&statutory, at("2026-03-29T00:00:00Z"), 1)
            .unwrap_err()
            .to_string()
            .contains("horizon"));
    }

    #[test]
    fn official_apia_skipped_date_mapper_has_no_open_piece_without_claiming_2011_coverage() {
        let evidence: serde_json::Value =
            serde_json::from_slice(include_bytes!("data/tzdb/2026e/apia-2011-test.json")).unwrap();
        assert_eq!(
            evidence["source_sha256"],
            trusted_data().unwrap().manifest["source_sha256"]
        );
        let split = evidence["transitions"][0]["at_utc_ms"].as_i64().unwrap();
        assert_eq!(split, at("2011-12-30T10:00:00Z"));
        let skipped = date("2011-12-30").unwrap();
        let window = WorkWindow {
            weekday: 5,
            start_minute: 0,
            end_minute: 1440,
        };
        let start = evidence["horizon_start_ms"].as_i64().unwrap();
        let end = evidence["horizon_end_ms"].as_i64().unwrap();
        assert_eq!(
            window_intersection(
                start,
                split,
                evidence["initial_offset_seconds"].as_i64().unwrap() as i32,
                skipped,
                &window
            )
            .unwrap(),
            None
        );
        assert_eq!(
            window_intersection(
                split,
                end,
                evidence["transitions"][0]["offset_seconds"]
                    .as_i64()
                    .unwrap() as i32,
                skipped,
                &window
            )
            .unwrap(),
            None
        );
        let pin = mint_calendar_pin(&calendar(5, 0, 1440), "Pacific/Apia").unwrap();
        assert!(working_due(&pin, split, 1).is_err());
    }

    #[test]
    fn safety_caps_and_sparse_full_horizon_exhaustion_are_visible_errors() {
        let mut config = calendar(1, 0, 1);
        config.weekly_windows = (1..=7)
            .flat_map(|weekday| {
                (0..8).map(move |slot| WorkWindow {
                    weekday,
                    start_minute: slot * 2,
                    end_minute: slot * 2 + 1,
                })
            })
            .collect();
        let pin = mint_calendar_pin(&config, "Europe/Warsaw").unwrap();
        let error = working_due(&pin, at("2023-12-31T23:00:00Z"), 31536000)
            .unwrap_err()
            .to_string();
        assert!(error.contains("horizon"), "{error}");
        assert!(
            !error.contains("budget"),
            "a valid full-horizon traversal fits the exact bounded counters"
        );
        config.weekly_windows.push(WorkWindow {
            weekday: 7,
            start_minute: 16,
            end_minute: 17,
        });
        assert!(validate_work_calendar(&config).is_err());
        let mut overlapping = calendar(1, 60, 120);
        overlapping.weekly_windows.push(WorkWindow {
            weekday: 1,
            start_minute: 90,
            end_minute: 150,
        });
        assert!(validate_work_calendar(&overlapping).is_err());
        let valid = mint_calendar_pin(&calendar(1, 0, 1440), "UTC").unwrap();
        for (anchor, seconds) in [
            (at("2023-12-31T00:00:00Z"), 1),
            (at("2041-01-01T00:00:00Z"), 1),
            (i64::MAX, 1),
            (i64::MIN, 1),
            (at("2026-01-05T00:00:00Z"), 0),
            (at("2026-01-05T00:00:00Z"), 31536001),
        ] {
            assert!(working_due(&valid, anchor, seconds).is_err());
        }
        let last_day = mint_calendar_pin(&calendar(1, 0, 1440), "UTC").unwrap();
        assert_eq!(
            working_due(&last_day, at("2040-12-31T23:59:59Z"), 1).unwrap(),
            at("2041-01-01T00:00:00Z")
        );
    }
}
