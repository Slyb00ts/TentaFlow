// =============================================================================
// File: tentaflow-cli/src/commands/user.rs
// Purpose: `tentaflow-cli user assign-role` — single subcommand that mints (or
//          replaces) a (user, org) -> role membership row. The same data path
//          as `org invite`; this command exists so admin runbooks that think
//          in terms of "give this user a role" do not have to context-switch
//          to org-centric verbs. Replace semantics: if the user already has a
//          different role in the same org, the existing row is removed first
//          and the new role is granted under the same `granted_by` identity.
// =============================================================================

use clap::Subcommand;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tentaflow_core::db;
use tentaflow_core::services::org::{repo, OrgError};

const ALLOWED_ROLE_NAMES: &[&str] =
    &["org_admin", "org_operator", "org_viewer", "dpo", "supervisor"];

#[derive(Subcommand, Debug)]
pub enum UserCommand {
    /// Grant (or replace) a role for a user within an organization. Replaces
    /// any existing membership for the same (user, org) pair.
    AssignRole {
        user_id: String,
        org_id: String,
        /// Role name — one of org_admin / org_operator / org_viewer / dpo /
        /// supervisor (or a literal role_id starting with `role-`).
        #[arg(long)]
        role: String,
        #[arg(long = "granted-by", default_value = "cli")]
        granted_by: String,
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
}

pub fn run(cmd: UserCommand) -> ExitCode {
    match cmd {
        UserCommand::AssignRole {
            user_id,
            org_id,
            role,
            granted_by,
            db,
        } => run_assign_role(&user_id, &org_id, &role, &granted_by, &db),
    }
}

fn open_db(path: &Path) -> Result<tentaflow_core::db::DbPool, ExitCode> {
    db::init(path).map_err(|e| {
        eprintln!("Cannot open DB {}: {e}", path.display());
        ExitCode::from(1)
    })
}

fn resolve_role_id(pool: &tentaflow_core::db::DbPool, role: &str) -> Result<String, ExitCode> {
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

fn run_assign_role(
    user_id: &str,
    org_id: &str,
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
    // `add_membership` is INSERT OR IGNORE — a user who already has a row in
    // this org keeps their existing role. Implement replace semantics by
    // removing first, then re-adding. The cache invalidation that
    // `remove_membership` triggers covers the swap.
    if let Err(e) = repo::remove_membership(&pool, org_id, user_id) {
        eprintln!("Error: {e}");
        return ExitCode::from(1);
    }
    match repo::add_membership(&pool, org_id, user_id, &role_id, granted_by) {
        Ok(_) => {
            println!("OK: user '{user_id}' assigned role '{role}' in org '{org_id}'");
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
