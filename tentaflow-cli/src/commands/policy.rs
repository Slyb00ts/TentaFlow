// =============================================================================
// File: tentaflow-cli/src/commands/policy.rs
// Purpose: `tentaflow-cli policy {issue,list,verify,revoke,show}` — admin
//          lifecycle for DPIA / FRIA / legal-grant / consent claims used by
//          the F1c P4 policy/claims engine. Claim issuance is admin-only:
//          the addon-facing surface is read-only (gate_check_v1).
// =============================================================================

use clap::Subcommand;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tentaflow_core::addon::host_functions::gate::{
    primary_claim_type_for_gate, required_signer_roles_for_gate,
};
use tentaflow_core::addon::lifecycle::parse_manifest_toml;
use tentaflow_core::db;
use tentaflow_core::services::policy::{
    self, ClaimContext, ListFilter, NewClaim, NewSignature, PolicyError,
};

const ALLOWED_TYPES: &[&str] = &[
    "dpia",
    "fria",
    "legal_grant",
    "consent",
    "approval",
    "grant",
    "deployment_profile",
];

#[derive(Subcommand, Debug)]
pub enum PolicyCommand {
    /// Issue a new claim (DPIA / FRIA / legal grant / consent).
    Issue {
        /// Globally unique claim id (e.g. "claim-dpia-faces-2026").
        #[arg(long = "claim-id")]
        claim_id: String,
        /// Claim type — one of dpia / fria / legal_grant / consent / approval
        /// / grant / deployment_profile.
        #[arg(long = "type")]
        claim_type: String,
        /// Human-readable label for the admin UI.
        #[arg(long)]
        label: String,
        /// Optional URI to the source document (DPIA PDF, FRIA assessment).
        #[arg(long = "document-uri")]
        document_uri: Option<String>,
        /// Optional addon scope — claim only valid for this addon.
        #[arg(long = "scope-addon")]
        scope_addon: Option<String>,
        /// Optional resource scope (vector namespace / alias id).
        #[arg(long = "scope-namespace")]
        scope_namespace: Option<String>,
        /// Validity window start (UTC ISO-8601, default: now).
        #[arg(long = "valid-from")]
        valid_from: Option<String>,
        /// Validity window end (UTC ISO-8601). Mandatory — claims always expire.
        #[arg(long = "valid-until")]
        valid_until: String,
        /// Admin user identity recording the issuance.
        #[arg(long = "issued-by", default_value = "admin")]
        issued_by: String,
        /// Required signers in `role:user` format. Repeat for multi-sig
        /// (e.g. `--signer dpo:alice --signer supervisor:bob`).
        #[arg(long = "signer", value_name = "ROLE:USER")]
        signer: Vec<String>,
        /// DB path.
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// List existing claims (newest first).
    List {
        /// Filter by claim type.
        #[arg(long = "type")]
        claim_type: Option<String>,
        /// Hide revoked + expired claims.
        #[arg(long = "active-only", default_value_t = false)]
        active_only: bool,
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// Show full details of one claim including signatures.
    Show {
        #[arg(long = "claim-id")]
        claim_id: String,
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// Verify a claim against a gate context (dry-run, no mutation).
    ///
    /// Two ways to declare what the gate requires:
    ///   * `--addon-manifest <path> --gate-id <id>` — canonical: reads the
    ///     `[[gate]]` block from the addon manifest. This is what the runtime
    ///     enforces, so admins should prefer this when they want to mirror
    ///     production behavior.
    ///   * `--type` + `--signer-role` — manual override. Used when no manifest
    ///     is available (rare) or to probe a hypothetical gate spec.
    /// When both are given the manifest wins (canonical source) and `--type`
    /// + `--signer-role` are ignored to avoid a silent false-positive OK.
    Verify {
        #[arg(long = "claim-id")]
        claim_id: String,
        /// Expected claim type. Ignored when `--addon-manifest` + `--gate-id`
        /// are supplied (manifest gate spec wins).
        #[arg(long = "type", default_value = "")]
        claim_type: String,
        /// Addon identity used for scope matching.
        #[arg(long = "addon", default_value = "")]
        addon: String,
        /// Optional resource scope (vector namespace / alias id).
        #[arg(long = "namespace")]
        namespace: Option<String>,
        /// Required signer roles. Repeat for multi-role. Default: dpo.
        /// Ignored when `--addon-manifest` + `--gate-id` are supplied.
        #[arg(long = "signer-role")]
        signer_role: Vec<String>,
        /// Path to an addon manifest.toml. When combined with `--gate-id`,
        /// the engine pulls the required signer roles and claim type from
        /// the `[[gate]]` block instead of `--signer-role` / `--type`.
        #[arg(long = "addon-manifest")]
        addon_manifest: Option<PathBuf>,
        /// Gate id from the addon manifest `[[gate]]` block. Required when
        /// `--addon-manifest` is supplied.
        #[arg(long = "gate-id")]
        gate_id: Option<String>,
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// Revoke a claim with a reason — gates referencing it deny from now on.
    Revoke {
        #[arg(long = "claim-id")]
        claim_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
}

pub fn run(cmd: PolicyCommand) -> ExitCode {
    match cmd {
        PolicyCommand::Issue {
            claim_id,
            claim_type,
            label,
            document_uri,
            scope_addon,
            scope_namespace,
            valid_from,
            valid_until,
            issued_by,
            signer,
            db,
        } => run_issue(
            &claim_id,
            &claim_type,
            &label,
            document_uri.as_deref(),
            scope_addon.as_deref(),
            scope_namespace.as_deref(),
            valid_from.as_deref(),
            &valid_until,
            &issued_by,
            &signer,
            &db,
        ),
        PolicyCommand::List {
            claim_type,
            active_only,
            db,
        } => run_list(claim_type.as_deref(), active_only, &db),
        PolicyCommand::Show { claim_id, db } => run_show(&claim_id, &db),
        PolicyCommand::Verify {
            claim_id,
            claim_type,
            addon,
            namespace,
            signer_role,
            addon_manifest,
            gate_id,
            db,
        } => run_verify(
            &claim_id,
            &claim_type,
            &addon,
            namespace.as_deref(),
            &signer_role,
            addon_manifest.as_deref(),
            gate_id.as_deref(),
            &db,
        ),
        PolicyCommand::Revoke {
            claim_id,
            reason,
            db,
        } => run_revoke(&claim_id, &reason, &db),
    }
}

fn open_db(path: &Path) -> Result<tentaflow_core::db::DbPool, ExitCode> {
    db::init(path).map_err(|e| {
        eprintln!("Cannot open DB {}: {e}", path.display());
        ExitCode::from(1)
    })
}

#[allow(clippy::too_many_arguments)]
fn run_issue(
    claim_id: &str,
    claim_type: &str,
    label: &str,
    document_uri: Option<&str>,
    scope_addon: Option<&str>,
    scope_namespace: Option<&str>,
    valid_from: Option<&str>,
    valid_until: &str,
    issued_by: &str,
    signers: &[String],
    db_path: &Path,
) -> ExitCode {
    if !ALLOWED_TYPES.contains(&claim_type) {
        eprintln!(
            "Error: --type '{claim_type}' is not allowed. Allowed: {}",
            ALLOWED_TYPES.join(", ")
        );
        return ExitCode::from(1);
    }
    if label.trim().is_empty() {
        eprintln!("Error: --label cannot be empty");
        return ExitCode::from(1);
    }
    if signers.is_empty() {
        eprintln!("Error: at least one --signer required (format: role:user)");
        return ExitCode::from(1);
    }
    let parsed_signers: Result<Vec<(String, String)>, String> = signers
        .iter()
        .map(|s| {
            let (role, user) = s
                .split_once(':')
                .ok_or_else(|| format!("invalid --signer '{s}' (expected role:user)"))?;
            if role.is_empty() || user.is_empty() {
                return Err(format!("invalid --signer '{s}' (role/user empty)"));
            }
            Ok((role.to_string(), user.to_string()))
        })
        .collect();
    let parsed_signers = match parsed_signers {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };

    let now = chrono::Utc::now().to_rfc3339();
    let valid_from_resolved = valid_from.map(String::from).unwrap_or_else(|| now.clone());

    // Sanity check on valid_until > valid_from
    if valid_until.as_bytes() <= valid_from_resolved.as_bytes() {
        eprintln!("Error: --valid-until must be strictly after --valid-from");
        return ExitCode::from(1);
    }

    let pool = match open_db(db_path) {
        Ok(p) => p,
        Err(c) => return c,
    };

    let claim = NewClaim {
        claim_id: claim_id.to_string(),
        claim_type: claim_type.to_string(),
        label: label.to_string(),
        subject: None,
        scope: None,
        document_uri: document_uri.map(String::from),
        scope_addon_id: scope_addon.map(String::from),
        scope_namespace: scope_namespace.map(String::from),
        valid_from: valid_from_resolved,
        valid_until: valid_until.to_string(),
        issued_by_user: issued_by.to_string(),
        created_at: now.clone(),
    };

    if let Err(e) = policy::insert_claim(&pool, &claim) {
        eprintln!("Error inserting claim: {e}");
        return ExitCode::from(1);
    }
    for (role, user) in &parsed_signers {
        let sig = NewSignature {
            claim_id: claim_id.to_string(),
            signer_role: role.clone(),
            signer_user: user.clone(),
            signed_at: now.clone(),
            signature_b64: None,
        };
        if let Err(e) = policy::insert_signature(&pool, &sig) {
            eprintln!("Error inserting signature ({role}:{user}): {e}");
            return ExitCode::from(1);
        }
    }
    println!(
        "OK: claim '{claim_id}' issued ({claim_type}), {} signer(s)",
        parsed_signers.len()
    );
    ExitCode::SUCCESS
}

fn run_list(claim_type: Option<&str>, active_only: bool, db_path: &Path) -> ExitCode {
    let pool = match open_db(db_path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    let now = chrono::Utc::now().to_rfc3339();
    let filter = ListFilter {
        claim_type: claim_type.map(String::from),
        active_only,
        now_utc: if active_only { Some(now) } else { None },
    };
    match policy::list_claims(&pool, &filter) {
        Ok(rows) => {
            if rows.is_empty() {
                println!("(no claims match filter)");
            } else {
                println!(
                    "{:<28} {:<12} {:<24} {:<24} {:<10} {}",
                    "claim_id", "type", "valid_from", "valid_until", "revoked", "label"
                );
                for r in rows {
                    println!(
                        "{:<28} {:<12} {:<24} {:<24} {:<10} {}",
                        r.claim_id,
                        r.claim_type,
                        r.valid_from,
                        r.valid_until,
                        if r.revoked_at.is_some() { "yes" } else { "no" },
                        r.label
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

fn run_show(claim_id: &str, db_path: &Path) -> ExitCode {
    let pool = match open_db(db_path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    match policy::get_claim(&pool, claim_id) {
        Ok(Some(r)) => {
            println!("claim_id      : {}", r.claim_id);
            println!("type          : {}", r.claim_type);
            println!("label         : {}", r.label);
            println!("subject       : {}", r.subject.as_deref().unwrap_or("-"));
            println!("scope         : {}", r.scope.as_deref().unwrap_or("-"));
            println!("document_uri  : {}", r.document_uri.as_deref().unwrap_or("-"));
            println!(
                "scope_addon   : {}",
                r.scope_addon_id.as_deref().unwrap_or("(global)")
            );
            println!(
                "scope_namespc : {}",
                r.scope_namespace.as_deref().unwrap_or("(any)")
            );
            println!("valid_from    : {}", r.valid_from);
            println!("valid_until   : {}", r.valid_until);
            println!(
                "revoked_at    : {}",
                r.revoked_at.as_deref().unwrap_or("(active)")
            );
            println!(
                "revoked_reason: {}",
                r.revoked_reason.as_deref().unwrap_or("-")
            );
            println!("issued_by     : {}", r.issued_by_user);
            println!("created_at    : {}", r.created_at);
            match policy::list_signatures(&pool, claim_id) {
                Ok(sigs) => {
                    println!("signers       : {}", sigs.len());
                    for s in sigs {
                        let crypto = if s.signature_b64.is_some() {
                            "ed25519"
                        } else {
                            "manual"
                        };
                        println!("  - {:<14} {:<24} {} [{}]", s.signer_role, s.signer_user, s.signed_at, crypto);
                    }
                }
                Err(e) => eprintln!("Warning: listing signatures failed: {e}"),
            }
            ExitCode::SUCCESS
        }
        Ok(None) => {
            eprintln!("Error: claim '{claim_id}' not found");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_verify(
    claim_id: &str,
    claim_type: &str,
    addon: &str,
    namespace: Option<&str>,
    signer_roles: &[String],
    addon_manifest: Option<&Path>,
    gate_id: Option<&str>,
    db_path: &Path,
) -> ExitCode {
    let pool = match open_db(db_path) {
        Ok(p) => p,
        Err(c) => return c,
    };

    // Manifest path wins over manual --type / --signer-role to prevent a
    // silent false-positive OK when an admin under-declares signer roles
    // relative to what production gate enforcement actually requires.
    let (resolved_claim_type, resolved_roles) = match (addon_manifest, gate_id) {
        (Some(path), Some(g_id)) => {
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error: cannot read manifest {}: {e}", path.display());
                    return ExitCode::from(1);
                }
            };
            let manifest = match parse_manifest_toml(&content) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("Error: cannot parse manifest {}: {e}", path.display());
                    return ExitCode::from(1);
                }
            };
            let gate = match manifest.gates.iter().find(|g| g.id == g_id) {
                Some(g) => g,
                None => {
                    eprintln!(
                        "Error: gate '{g_id}' not declared in manifest {}",
                        path.display()
                    );
                    return ExitCode::from(1);
                }
            };
            (
                primary_claim_type_for_gate(gate),
                required_signer_roles_for_gate(gate),
            )
        }
        (Some(_), None) | (None, Some(_)) => {
            eprintln!("Error: --addon-manifest and --gate-id must be supplied together");
            return ExitCode::from(1);
        }
        (None, None) => {
            if claim_type.is_empty() {
                eprintln!("Error: --type is required when --addon-manifest is not supplied");
                return ExitCode::from(1);
            }
            let mut roles: Vec<String> = signer_roles.to_vec();
            if roles.is_empty() {
                roles.push("dpo".to_string());
            }
            (claim_type.to_string(), roles)
        }
    };

    let ctx = ClaimContext {
        addon_id: addon.to_string(),
        claim_type_required: resolved_claim_type,
        resource_scope: namespace.map(String::from),
        required_signer_roles: resolved_roles,
        now_utc: chrono::Utc::now().to_rfc3339(),
    };
    match policy::verify_claim(&pool, claim_id, &ctx) {
        Ok(v) => {
            println!("OK: claim '{}' valid", v.claim_id);
            println!("  type        : {}", v.claim_type);
            println!("  valid_until : {}", v.valid_until);
            println!("  signers     : {}", v.signers.len());
            for s in v.signers {
                println!("    - {} ({})", s.role, s.user);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let code = match e {
                PolicyError::ClaimNotFound(_) => "not_found",
                PolicyError::ClaimRevoked { .. } => "revoked",
                PolicyError::ClaimNotInValidityPeriod { .. } => "outside_validity",
                PolicyError::ClaimTypeMismatch { .. } => "type_mismatch",
                PolicyError::ClaimScopeMismatch { .. } => "scope_mismatch",
                PolicyError::MissingRequiredSigner { .. } => "missing_signer",
                PolicyError::DbError(_) => "db_error",
            };
            eprintln!("INVALID [{code}]: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_revoke(claim_id: &str, reason: &str, db_path: &Path) -> ExitCode {
    if reason.trim().is_empty() {
        eprintln!("Error: --reason cannot be empty");
        return ExitCode::from(1);
    }
    let pool = match open_db(db_path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    let now = chrono::Utc::now().to_rfc3339();
    match policy::revoke_claim(&pool, claim_id, reason, &now) {
        Ok(true) => {
            println!("OK: claim '{claim_id}' revoked");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            eprintln!("Warning: claim '{claim_id}' not found or already revoked");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
    }
}
