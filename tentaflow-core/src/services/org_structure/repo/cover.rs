//! Writes of deputies and absences (docs §2.2). A child module of `repo` so it
//! uses the write session (`Session`, `run`, the row helpers) without widening
//! their visibility.
//!
//! Who may write is decided here, not by the transport: an administrator
//! changes anything; a person adds, changes and deletes their OWN absences, but
//! only those they entered by hand. Deputies are the administrator's.
//!
//! The reason of an absence is private (§6.3): it never goes into an audit
//! summary or a sync log line, only into the row.

use super::*;
use crate::services::org_structure::availability::{
    absences_where, deputies_where, Absence, AbsenceKind, Deputy, DeputyScope, SOURCE_MANUAL,
};

/// Longest reason a person may write.
pub const MAX_REASON_CHARS: usize = 500;

#[derive(Debug, Clone)]
pub struct NewDeputy {
    pub user_id: String,
    pub deputy_user_id: String,
    pub scope: DeputyScope,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
}

/// `Some(None)` clears the end.
#[derive(Debug, Clone, Default)]
pub struct DeputyPatch {
    pub scope: Option<DeputyScope>,
    pub valid_from: Option<NaiveDate>,
    pub valid_to: Option<Option<NaiveDate>>,
}

#[derive(Debug, Clone)]
pub struct NewAbsence {
    pub user_id: String,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
    pub kind: AbsenceKind,
    pub reason: Option<String>,
}

/// `Some(None)` clears the end or the reason.
#[derive(Debug, Clone, Default)]
pub struct AbsencePatch {
    pub valid_from: Option<NaiveDate>,
    pub valid_to: Option<Option<NaiveDate>>,
    pub kind: Option<AbsenceKind>,
    pub reason: Option<Option<String>>,
}

/// Who is asking, for the rules of `set_absence`.
#[derive(Debug, Clone, Copy)]
pub struct Actor {
    pub is_admin: bool,
}

fn normalize_reason(reason: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = reason.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    if raw.chars().count() > MAX_REASON_CHARS {
        return Err(E::InvalidValue {
            field: "reason",
            reason: format!("longer than {MAX_REASON_CHARS} characters"),
        });
    }
    if raw.chars().any(|c| c.is_control() && c != '\n') {
        return Err(E::InvalidValue {
            field: "reason",
            reason: "control characters".into(),
        });
    }
    Ok(Some(raw.to_string()))
}

/// The earlier of two optional days, `None` only when both are open-ended.
/// A change from one end to another rewrites every day in between, so the
/// backdating rule looks at the earlier of the two.
fn earliest(a: Option<NaiveDate>, b: Option<NaiveDate>) -> Option<NaiveDate> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

/// Who may change a deputy: the covered person themselves, or an administrator.
/// A manager has no such right — the owner decided a substitution is the covered
/// person's own arrangement (docs §2.2, decision 2026-09-30).
pub fn ensure_may_write_deputy(actor: &str, is_admin: bool, covered_user_id: &str) -> Result<()> {
    if is_admin || actor == covered_user_id {
        Ok(())
    } else {
        Err(E::NotPermitted("a deputy of another person"))
    }
}

fn deputy_by_id(s: &Session<'_>, id: &str) -> Result<Deputy> {
    one(
        deputies_where(s.tx, "org_id = ?1 AND id = ?2", [s.org_id, id])?,
        "deputy",
        id,
    )
}

fn absence_by_id(s: &Session<'_>, id: &str) -> Result<Absence> {
    one(
        absences_where(s.tx, "org_id = ?1 AND id = ?2", [s.org_id, id])?,
        "absence",
        id,
    )
}

// ---------------------------------------------------------------------------
// Deputies
// ---------------------------------------------------------------------------

fn ensure_no_overlapping_deputy(
    s: &Session<'_>,
    user_id: &str,
    deputy_user_id: &str,
    scope: &DeputyScope,
    interval: &Interval,
    except_id: Option<&str>,
) -> Result<()> {
    let scope_wire = scope.as_wire();
    for row in s.rows_where(
        &repl::DEPUTIES,
        &[
            ("user_id", user_id),
            ("deputy_user_id", deputy_user_id),
            ("scope", &scope_wire),
        ],
    )? {
        if except_id == Some(row.text("id").as_str()) {
            continue;
        }
        if row.interval()?.overlaps(interval) {
            return Err(E::Duplicate {
                entity: "deputy",
                name: scope_wire,
            });
        }
    }
    Ok(())
}

pub fn set_deputy(pool: &DbPool, ctx: &WriteCtx<'_>, new: &NewDeputy) -> Result<Written<Deputy>> {
    run(pool, ctx, "org.deputy.set", |s| {
        set_deputy_in(s, ctx.actor_user_id, new)
    })
}

pub(in crate::services::org_structure) fn set_deputy_in(
    s: &mut Session<'_>,
    actor: &str,
    new: &NewDeputy,
) -> Result<Audited<Deputy>> {
    let interval = Interval::new(new.valid_from, new.valid_to)?;
    if new.user_id == new.deputy_user_id {
        return Err(E::InvalidValue {
            field: "deputy_user_id",
            reason: "a person cannot be their own deputy".into(),
        });
    }
    s.ensure_not_backdated(new.valid_from)?;
    s.require_user(&new.user_id)?;
    s.require_user(&new.deputy_user_id)?;
    ensure_no_overlapping_deputy(
        s,
        &new.user_id,
        &new.deputy_user_id,
        &new.scope,
        &interval,
        None,
    )?;
    let mut row = s.new_row(&repl::DEPUTIES);
    let id = row.text("id");
    row.set("user_id", text(&new.user_id));
    row.set("deputy_user_id", text(&new.deputy_user_id));
    row.set("scope", text(&new.scope.as_wire()));
    row.set("valid_from", date_value(new.valid_from));
    row.set("valid_to", opt_date_value(new.valid_to));
    row.set("created_by", text(actor));
    s.insert(&repl::DEPUTIES, &row)?;
    Ok(Audited {
        value: deputy_by_id(s, &id)?,
        target: format!("org_deputy:{id}"),
        summary: json!({
            "user_id": new.user_id,
            "deputy_user_id": new.deputy_user_id,
            "scope": new.scope.as_wire(),
            "valid_from": fmt(new.valid_from),
            "valid_to": opt_fmt(new.valid_to),
        }),
    })
}

pub fn update_deputy(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    id: &str,
    patch: &DeputyPatch,
) -> Result<Written<Deputy>> {
    run(pool, ctx, "org.deputy.update", |s| {
        update_deputy_in(s, id, patch)
    })
}

fn update_deputy_in(s: &mut Session<'_>, id: &str, patch: &DeputyPatch) -> Result<Audited<Deputy>> {
    s.require_in_org("org_deputies", "deputy", "id", id)?;
    let before = deputy_by_id(s, id)?;
    let scope = patch.scope.clone().unwrap_or_else(|| before.scope.clone());
    let from = patch.valid_from.unwrap_or(before.valid_from);
    let to = patch.valid_to.unwrap_or(before.valid_to);
    if patch.scope.is_none() && patch.valid_from.is_none() && patch.valid_to.is_none() {
        return Err(E::InvalidValue {
            field: "patch",
            reason: "nothing to change".into(),
        });
    }
    let interval = Interval::new(from, to)?;
    if from != before.valid_from {
        s.ensure_not_backdated(from.min(before.valid_from))?;
    }
    if to != before.valid_to {
        if let Some(day) = earliest(to, before.valid_to) {
            s.ensure_not_backdated(day)?;
        }
    }
    ensure_no_overlapping_deputy(
        s,
        &before.user_id,
        &before.deputy_user_id,
        &scope,
        &interval,
        Some(id),
    )?;
    s.set_cols(
        &repl::DEPUTIES,
        id,
        &[
            ("scope", text(&scope.as_wire())),
            ("valid_from", date_value(from)),
            ("valid_to", opt_date_value(to)),
        ],
    )?;
    Ok(Audited {
        value: deputy_by_id(s, id)?,
        target: format!("org_deputy:{id}"),
        summary: json!({
            "scope": scope.as_wire(),
            "valid_from": fmt(from),
            "valid_to": opt_fmt(to),
        }),
    })
}

/// Ends the deputy from `from`: covering stops on that day. A deputy that would
/// end on or before its first day never covered anybody and is removed.
pub fn end_deputy(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    id: &str,
    from: NaiveDate,
) -> Result<Written<()>> {
    run(pool, ctx, "org.deputy.end", |s| end_deputy_in(s, id, from))
}

pub(in crate::services::org_structure) fn end_deputy_in(
    s: &mut Session<'_>,
    id: &str,
    from: NaiveDate,
) -> Result<Audited<()>> {
    s.require_in_org("org_deputies", "deputy", "id", id)?;
    let row = deputy_by_id(s, id)?;
    if row.valid_to.is_some_and(|end| from >= end) {
        return Err(E::NotValidAt {
            entity: "deputy",
            id: id.to_string(),
            date: fmt(from),
        });
    }
    s.ensure_not_backdated(from)?;
    let removed = from <= row.valid_from;
    if removed {
        s.delete(&repl::DEPUTIES, id)?;
    } else {
        s.set_cols(&repl::DEPUTIES, id, &[("valid_to", date_value(from))])?;
    }
    Ok(Audited {
        value: (),
        target: format!("org_deputy:{id}"),
        summary: removal_summary(from, removed),
    })
}

// ---------------------------------------------------------------------------
// Absences
// ---------------------------------------------------------------------------

fn ensure_may_write_absence(actor: &str, is_admin: bool, absence: &Absence) -> Result<()> {
    if is_admin {
        return Ok(());
    }
    if absence.user_id != actor {
        return Err(E::NotPermitted("an absence of another person"));
    }
    if absence.source != SOURCE_MANUAL {
        return Err(E::NotPermitted("an absence that came from another source"));
    }
    Ok(())
}

pub fn add_absence(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    who: Actor,
    new: &NewAbsence,
) -> Result<Written<Absence>> {
    run(pool, ctx, "org.absence.add", |s| {
        add_absence_in(s, ctx.actor_user_id, who, new)
    })
}

fn add_absence_in(
    s: &mut Session<'_>,
    actor: &str,
    who: Actor,
    new: &NewAbsence,
) -> Result<Audited<Absence>> {
    if new.user_id != actor && !who.is_admin {
        return Err(E::NotPermitted("an absence of another person"));
    }
    Interval::new(new.valid_from, new.valid_to)?;
    let reason = normalize_reason(new.reason.as_deref())?;
    s.ensure_not_backdated(new.valid_from)?;
    s.require_user(&new.user_id)?;
    let mut row = s.new_row(&repl::ABSENCES);
    let id = row.text("id");
    row.set("user_id", text(&new.user_id));
    row.set("valid_from", date_value(new.valid_from));
    row.set("valid_to", opt_date_value(new.valid_to));
    row.set("kind", text(new.kind.as_str()));
    row.set("reason", opt(reason.as_deref()));
    row.set("source", text(SOURCE_MANUAL));
    row.set("created_by", text(actor));
    s.insert(&repl::ABSENCES, &row)?;
    Ok(Audited {
        value: absence_by_id(s, &id)?,
        target: format!("org_absence:{id}"),
        summary: json!({
            "user_id": new.user_id,
            "kind": new.kind.as_str(),
            "has_reason": reason.is_some(),
            "valid_from": fmt(new.valid_from),
            "valid_to": opt_fmt(new.valid_to),
        }),
    })
}

pub fn update_absence(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    who: Actor,
    id: &str,
    patch: &AbsencePatch,
) -> Result<Written<Absence>> {
    run(pool, ctx, "org.absence.update", |s| {
        update_absence_in(s, ctx.actor_user_id, who, id, patch)
    })
}

fn update_absence_in(
    s: &mut Session<'_>,
    actor: &str,
    who: Actor,
    id: &str,
    patch: &AbsencePatch,
) -> Result<Audited<Absence>> {
    s.require_in_org("org_absences", "absence", "id", id)?;
    let before = absence_by_id(s, id)?;
    ensure_may_write_absence(actor, who.is_admin, &before)?;
    if patch.valid_from.is_none()
        && patch.valid_to.is_none()
        && patch.kind.is_none()
        && patch.reason.is_none()
    {
        return Err(E::InvalidValue {
            field: "patch",
            reason: "nothing to change".into(),
        });
    }
    let from = patch.valid_from.unwrap_or(before.valid_from);
    let to = patch.valid_to.unwrap_or(before.valid_to);
    Interval::new(from, to)?;
    let kind = patch.kind.unwrap_or(before.kind);
    let reason = match &patch.reason {
        Some(new) => normalize_reason(new.as_deref())?,
        None => before.reason.clone(),
    };
    if from != before.valid_from {
        s.ensure_not_backdated(from.min(before.valid_from))?;
    }
    if to != before.valid_to {
        if let Some(day) = earliest(to, before.valid_to) {
            s.ensure_not_backdated(day)?;
        }
    }
    s.set_cols(
        &repl::ABSENCES,
        id,
        &[
            ("valid_from", date_value(from)),
            ("valid_to", opt_date_value(to)),
            ("kind", text(kind.as_str())),
            ("reason", opt(reason.as_deref())),
        ],
    )?;
    Ok(Audited {
        value: absence_by_id(s, id)?,
        target: format!("org_absence:{id}"),
        summary: json!({
            "kind": kind.as_str(),
            "has_reason": reason.is_some(),
            "valid_from": fmt(from),
            "valid_to": opt_fmt(to),
        }),
    })
}

/// Removes the absence. One that has already begun rewrites days that were
/// recorded as away, so it asks for the backdating confirmation.
pub fn delete_absence(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    who: Actor,
    id: &str,
) -> Result<Written<()>> {
    run(pool, ctx, "org.absence.delete", |s| {
        delete_absence_in(s, ctx.actor_user_id, who, id)
    })
}

fn delete_absence_in(
    s: &mut Session<'_>,
    actor: &str,
    who: Actor,
    id: &str,
) -> Result<Audited<()>> {
    s.require_in_org("org_absences", "absence", "id", id)?;
    let row = absence_by_id(s, id)?;
    ensure_may_write_absence(actor, who.is_admin, &row)?;
    s.ensure_not_backdated(row.valid_from)?;
    s.delete(&repl::ABSENCES, id)?;
    Ok(Audited {
        value: (),
        target: format!("org_absence:{id}"),
        summary: json!({
            "user_id": row.user_id,
            "valid_from": fmt(row.valid_from),
            "valid_to": opt_fmt(row.valid_to),
        }),
    })
}
