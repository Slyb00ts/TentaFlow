// =============================================================================
// File: tentaflow-cli/src/commands/org.rs
// Purpose: `tentaflow-cli org {create,list,show,invite,remove,members,delete}`
//          — admin lifecycle for the F2 P1 multi-tenant org/membership graph.
//          Mirrors the policy CLI shape (open-db -> repo call -> formatted
//          output, exit code maps cleanly: 0 ok, 1 generic error / not found,
//          2 conflict). Read paths print as a fixed-width table; mutations
//          print a single `OK:` line for shell-script consumption.
// =============================================================================

use clap::Subcommand;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tentaflow_core::db;
use tentaflow_core::services::org::{repo, OrgError, DEFAULT_ORG_ID, DEFAULT_ORG_SLUG};
use tentaflow_core::services::rbac::PermissionMatrix;

/// Five roles seeded by migration v32. Used only for `--role` validation: an
/// unknown role short-circuits with an exit-1 message before the repo call
/// turns it into a `RoleNotFound` error (which would still work, but this is
/// a friendlier message).
const ALLOWED_ROLE_NAMES: &[&str] =
    &["org_admin", "org_operator", "org_viewer", "dpo", "supervisor"];

#[derive(Subcommand, Debug)]
pub enum OrgCommand {
    /// Create a new organization. Slug must be unique — collision returns
    /// exit code 2.
    Create {
        /// Human-readable name.
        name: String,
        /// URL-safe slug (lowercase, [a-z0-9-]). Must not collide with an
        /// existing org.
        #[arg(long)]
        slug: String,
        /// Optional contact e-mail surfaced in the admin UI.
        #[arg(long = "contact-email")]
        contact_email: Option<String>,
        /// Optional Data Protection Officer contact (e-mail / phone).
        #[arg(long = "dpo-contact")]
        dpo_contact: Option<String>,
        /// Optional retention policy as a JSON blob (passed through verbatim
        /// — the schema is validated by the dashboard at render time).
        #[arg(long = "retention-policy-json")]
        retention_policy_json: Option<String>,
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// List organizations. Default returns every status; `--status` filters
    /// to `active|suspended|deleted`.
    List {
        /// Filter by status.
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// Show full details of one org.
    Show {
        org_id: String,
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// Invite a user into an org with a named role. Idempotent — re-inviting
    /// an existing member is a no-op (exit 0). The user_id is the same string
    /// the dashboard uses (typically `iam.users.id` decimal as text).
    Invite {
        org_id: String,
        user_id: String,
        /// Role name — one of org_admin / org_operator / org_viewer / dpo /
        /// supervisor.
        #[arg(long)]
        role: String,
        /// Identity recording who granted the membership. Default: "cli".
        #[arg(long = "granted-by", default_value = "cli")]
        granted_by: String,
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// Remove a member from an org. Idempotent (missing member -> exit 0).
    Remove {
        org_id: String,
        user_id: String,
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// List memberships in an org (one row per (user, role) pair).
    Members {
        org_id: String,
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// Soft-delete an org (status -> 'deleted'). `--force` skips the
    /// membership-empty check; without it the CLI refuses to delete an org
    /// that still has at least one member (exit 2, the same code as a slug
    /// conflict — both are "you tried to mutate but state says no").
    Delete {
        org_id: String,
        #[arg(long, default_value_t = false)]
        force: bool,
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
}

pub fn run(cmd: OrgCommand) -> ExitCode {
    match cmd {
        OrgCommand::Create {
            name,
            slug,
            contact_email,
            dpo_contact,
            retention_policy_json,
            db,
        } => run_create(
            &name,
            &slug,
            contact_email.as_deref(),
            dpo_contact.as_deref(),
            retention_policy_json.as_deref(),
            &db,
        ),
        OrgCommand::List { status, db } => run_list(status.as_deref(), &db),
        OrgCommand::Show { org_id, db } => run_show(&org_id, &db),
        OrgCommand::Invite {
            org_id,
            user_id,
            role,
            granted_by,
            db,
        } => run_invite(&org_id, &user_id, &role, &granted_by, &db),
        OrgCommand::Remove {
            org_id,
            user_id,
            db,
        } => run_remove(&org_id, &user_id, &db),
        OrgCommand::Members { org_id, db } => run_members(&org_id, &db),
        OrgCommand::Delete {
            org_id,
            force,
            db,
        } => run_delete(&org_id, force, &db),
    }
}

fn open_db(path: &Path) -> Result<tentaflow_core::db::DbPool, ExitCode> {
    db::init(path).map_err(|e| {
        eprintln!("Cannot open DB {}: {e}", path.display());
        ExitCode::from(1)
    })
}

fn run_create(
    name: &str,
    slug: &str,
    contact_email: Option<&str>,
    dpo_contact: Option<&str>,
    retention_policy_json: Option<&str>,
    db_path: &Path,
) -> ExitCode {
    if name.trim().is_empty() {
        eprintln!("Error: name cannot be empty");
        return ExitCode::from(1);
    }
    if slug.trim().is_empty() {
        eprintln!("Error: --slug cannot be empty");
        return ExitCode::from(1);
    }
    if slug == DEFAULT_ORG_SLUG {
        // The default slug is reserved for the seed row created by migration
        // v32. Refusing it here prevents an admin from accidentally trying to
        // create a "second default" that would clash with the seed.
        eprintln!(
            "Error: --slug '{DEFAULT_ORG_SLUG}' is reserved for the default seed org"
        );
        return ExitCode::from(2);
    }

    let pool = match open_db(db_path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    match repo::create_organization(
        &pool,
        name,
        slug,
        contact_email,
        dpo_contact,
        retention_policy_json,
        None,
    ) {
        Ok(org) => {
            println!("OK: org '{}' created (slug='{}')", org.org_id, org.slug);
            ExitCode::SUCCESS
        }
        Err(OrgError::SlugConflict(s)) => {
            eprintln!("Error: slug '{s}' is already in use");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_list(status: Option<&str>, db_path: &Path) -> ExitCode {
    let pool = match open_db(db_path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    match repo::list_organizations(&pool, status) {
        Ok(rows) => {
            if rows.is_empty() {
                println!("(no organizations match filter)");
            } else {
                println!(
                    "{:<38} {:<24} {:<14} {:<10} {}",
                    "org_id", "slug", "status", "members", "name"
                );
                for o in rows {
                    let members = repo::list_memberships_for_org(&pool, &o.org_id)
                        .map(|v| v.len())
                        .unwrap_or(0);
                    println!(
                        "{:<38} {:<24} {:<14} {:<10} {}",
                        o.org_id, o.slug, o.status, members, o.name
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_show(org_id: &str, db_path: &Path) -> ExitCode {
    let pool = match open_db(db_path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    match repo::get_organization(&pool, org_id) {
        Ok(Some(o)) => {
            println!("org_id        : {}", o.org_id);
            println!("name          : {}", o.name);
            println!("slug          : {}", o.slug);
            println!(
                "contact_email : {}",
                o.contact_email.as_deref().unwrap_or("-")
            );
            println!(
                "dpo_contact   : {}",
                o.dpo_contact.as_deref().unwrap_or("-")
            );
            println!(
                "retention_json: {}",
                o.retention_policy_json.as_deref().unwrap_or("-")
            );
            println!("status        : {}", o.status);
            println!("created_at    : {}", o.created_at);
            let members = repo::list_memberships_for_org(&pool, &o.org_id).unwrap_or_default();
            println!("members       : {}", members.len());
            for (user_id, role) in members {
                println!("  - {:<32} {}", user_id, role.name);
            }
            ExitCode::SUCCESS
        }
        Ok(None) => {
            eprintln!("Error: org '{org_id}' not found");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
    }
}

fn resolve_role_id(pool: &tentaflow_core::db::DbPool, role: &str) -> Result<String, ExitCode> {
    // Accept either the human-readable role name (the common case for an
    // admin typing on the CLI — `--role org_admin`) or the literal role_id
    // (the seed ids are `role-org-admin` etc.). Anything else is rejected
    // before the repo call so the error message names the allowed values.
    if role.starts_with("role-") {
        return Ok(role.to_string());
    }
    if !ALLOWED_ROLE_NAMES.contains(&role) {
        eprintln!(
            "Error: unknown role '{role}'. Allowed: {}",
            ALLOWED_ROLE_NAMES.join(", ")
        );
        return Err(ExitCode::from(1));
    }
    let roles = repo::list_roles(pool).map_err(|e| {
        eprintln!("Error: cannot read roles table: {e}");
        ExitCode::from(1)
    })?;
    roles
        .into_iter()
        .find(|r| r.name == role)
        .map(|r| r.role_id)
        .ok_or_else(|| {
            eprintln!("Error: role '{role}' is not seeded in this DB (run migrations)");
            ExitCode::from(1)
        })
}

fn run_invite(
    org_id: &str,
    user_id: &str,
    role: &str,
    granted_by: &str,
    db_path: &Path,
) -> ExitCode {
    if user_id.trim().is_empty() {
        eprintln!("Error: user_id cannot be empty");
        return ExitCode::from(1);
    }
    let pool = match open_db(db_path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    let role_id = match resolve_role_id(&pool, role) {
        Ok(r) => r,
        Err(c) => return c,
    };
    match repo::add_membership(&pool, org_id, user_id, &role_id, granted_by) {
        Ok(true) => {
            println!("OK: user '{user_id}' invited to org '{org_id}' as '{role}'");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            // INSERT OR IGNORE returns false when the row already exists;
            // exit 0 so scripts can call this repeatedly.
            println!(
                "OK: user '{user_id}' already a member of org '{org_id}' (no change)"
            );
            ExitCode::SUCCESS
        }
        Err(OrgError::NotFound(id)) => {
            eprintln!("Error: org '{id}' not found");
            ExitCode::from(1)
        }
        Err(OrgError::RoleNotFound(id)) => {
            eprintln!("Error: role '{id}' not found");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_remove(org_id: &str, user_id: &str, db_path: &Path) -> ExitCode {
    let pool = match open_db(db_path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    match repo::remove_membership(&pool, org_id, user_id) {
        Ok(true) => {
            println!("OK: user '{user_id}' removed from org '{org_id}'");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            // Idempotent — pretend success so scripts can re-run safely.
            println!("OK: user '{user_id}' was not a member of org '{org_id}' (no change)");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_members(org_id: &str, db_path: &Path) -> ExitCode {
    let pool = match open_db(db_path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    // Verify the org exists so an empty list is not confused with "ghost
    // org id" — the former is a legitimate empty membership, the latter
    // should be exit 1.
    match repo::get_organization(&pool, org_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            eprintln!("Error: org '{org_id}' not found");
            return ExitCode::from(1);
        }
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    }
    match repo::list_memberships_for_org(&pool, org_id) {
        Ok(rows) => {
            if rows.is_empty() {
                println!("(no members in org '{org_id}')");
            } else {
                println!("{:<32} {:<18} {}", "user_id", "role", "role_id");
                for (user_id, role) in rows {
                    println!("{:<32} {:<18} {}", user_id, role.name, role.role_id);
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_delete(org_id: &str, force: bool, db_path: &Path) -> ExitCode {
    if org_id == DEFAULT_ORG_ID {
        eprintln!("Error: cannot delete the default seed org");
        return ExitCode::from(2);
    }
    let pool = match open_db(db_path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    if !force {
        let members = match repo::list_memberships_for_org(&pool, org_id) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error: {e}");
                return ExitCode::from(1);
            }
        };
        if !members.is_empty() {
            eprintln!(
                "Error: org '{org_id}' still has {} member(s); pass --force to soft-delete",
                members.len()
            );
            return ExitCode::from(2);
        }
    }
    // Soft-delete (status='deleted'); flush the permission cache so any
    // in-process resolver does not keep handing out the org_id past delete.
    let result = repo::delete_organization(&pool, org_id);
    // Per-(user, org) cache entries cannot be enumerated cheaply so flush
    // everything; the cost is a re-lookup on the next request per user.
    PermissionMatrix::global().invalidate_all();
    match result {
        Ok(true) => {
            println!("OK: org '{org_id}' soft-deleted (status='deleted')");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            eprintln!("Error: org '{org_id}' not found or already deleted");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
    }
}
