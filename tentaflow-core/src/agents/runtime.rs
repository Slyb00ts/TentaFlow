// ===== File: agents/runtime.rs — what an agent RUNS on (`agents.runtime_json`) =====
//
// An agent is either a model prompt loop (`kind = "llm"`, everything that
// existed before migration 156) or a CLI application driven through a provider
// account (`kind = "cli"`). The shape is the one the accepted G01 mockup edits:
//
//   {"kind":"llm"}
//   {"kind":"cli","engine":"claude-code","model":"…","reasoning":"…",
//    "account":{"mode":"global","account_id":"…"}}
//   {"kind":"cli","engine":"codex","account":{"mode":"user"}}
//
// `mode = "user"` resolves the account of whoever is RUNNING the agent, at every
// run, from the run's `AgentPrincipal` — which is why it cannot name an account
// here, and why naming one is refused rather than ignored. It never falls back
// to a global account: a run with no user account is a refusal the console turns
// into "connect your account", not a silent switch to somebody else's
// subscription.
//
// Nothing in here checks GRANTS. A grant can be added after the agent is saved,
// and an agent whose account lost its grant is still a valid agent — the
// refusal belongs to the run, not to the definition.

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::db::DbPool;
use crate::provider_accounts;

/// The runtime of an agent, parsed from `agents.runtime_json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRuntime {
    /// A model prompt loop. The historical agent, and the default every row
    /// migrated by v156 carries.
    Llm,
    Cli(CliRuntime),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliRuntime {
    pub engine: String,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub account: AccountBinding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountBinding {
    /// This exact global account, for every user who may run the agent.
    Global { account_id: String },
    /// The running user's own account for the engine, resolved per run.
    User,
}

/// `agents.runtime_json` for a plain LLM agent — the column default, and what a
/// caller that says nothing about a runtime means.
pub const LLM_RUNTIME_JSON: &str = r#"{"kind":"llm"}"#;

impl AgentRuntime {
    /// Parses and validates the JSON structurally: the enum values, the engine
    /// catalog, and the rule that a mode names an account exactly when it can
    /// have one. Whether the named account EXISTS needs the database and is
    /// [`validate_account_binding`].
    pub fn parse(runtime_json: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(runtime_json)
            .map_err(|e| anyhow!("agent runtime_json must be valid JSON: {e}"))?;
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("agent runtime_json must be a JSON object"))?;
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("agent runtime_json needs a 'kind' of 'llm' or 'cli'"))?;
        match kind {
            "llm" => Ok(AgentRuntime::Llm),
            "cli" => {
                let engine = object
                    .get("engine")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("a cli agent runtime needs an 'engine'"))?;
                if provider_accounts::engine(engine).is_none() {
                    return Err(anyhow!("unknown agent engine '{engine}'"));
                }
                let account = object
                    .get("account")
                    .and_then(Value::as_object)
                    .ok_or_else(|| anyhow!("a cli agent runtime needs an 'account'"))?;
                let mode = account
                    .get("mode")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("an agent account needs a 'mode'"))?;
                let account_id = account
                    .get("account_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty());
                let account = match (mode, account_id) {
                    ("global", Some(account_id)) => AccountBinding::Global {
                        account_id: account_id.to_string(),
                    },
                    ("global", None) => {
                        return Err(anyhow!("a global agent account must name the account"))
                    }
                    ("user", None) => AccountBinding::User,
                    ("user", Some(_)) => {
                        return Err(anyhow!(
                            "a user agent account is resolved from whoever runs the agent, so it \
                             cannot name one"
                        ))
                    }
                    (other, _) => {
                        return Err(anyhow!(
                            "agent account mode must be 'global' or 'user', not '{other}'"
                        ))
                    }
                };
                Ok(AgentRuntime::Cli(CliRuntime {
                    engine: engine.to_string(),
                    model: string_field(object, "model"),
                    reasoning: string_field(object, "reasoning"),
                    account,
                }))
            }
            other => Err(anyhow!(
                "agent runtime kind must be 'llm' or 'cli', not '{other}'"
            )),
        }
    }
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The half of the check that needs the registry: a global binding must name an
/// account that exists in this org and runs the SAME engine. An agent pointing
/// at a Codex account while claiming to run Claude Code would fail at the first
/// turn, with a refusal nobody could read back to this field.
pub fn validate_account_binding(db: &DbPool, org_id: &str, runtime: &AgentRuntime) -> Result<()> {
    let AgentRuntime::Cli(cli) = runtime else {
        return Ok(());
    };
    let AccountBinding::Global { account_id } = &cli.account else {
        return Ok(());
    };
    let account = provider_accounts::repository::get_account(db, account_id)?
        .ok_or_else(|| anyhow!("agent account '{account_id}' does not exist"))?;
    if account.org_id != org_id {
        return Err(anyhow!(
            "agent account '{account_id}' belongs to another organisation"
        ));
    }
    if account.scope != "global" {
        return Err(anyhow!(
            "agent account '{account_id}' is a personal account and cannot be assigned to an agent"
        ));
    }
    if account.engine_id != cli.engine {
        return Err(anyhow!(
            "agent account '{account_id}' runs {} but the agent runs {}",
            account.engine_id,
            cli.engine
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_runtime_is_a_plain_llm_agent() {
        assert_eq!(
            AgentRuntime::parse(LLM_RUNTIME_JSON).unwrap(),
            AgentRuntime::Llm
        );
    }

    #[test]
    fn a_cli_runtime_carries_its_engine_model_and_account() {
        let runtime = AgentRuntime::parse(
            r#"{"kind":"cli","engine":"claude-code","model":"sonnet","reasoning":"high",
                "account":{"mode":"global","account_id":"acc-1"}}"#,
        )
        .unwrap();
        assert_eq!(
            runtime,
            AgentRuntime::Cli(CliRuntime {
                engine: "claude-code".into(),
                model: Some("sonnet".into()),
                reasoning: Some("high".into()),
                account: AccountBinding::Global {
                    account_id: "acc-1".into()
                },
            })
        );
    }

    #[test]
    fn a_user_binding_names_no_account() {
        let runtime =
            AgentRuntime::parse(r#"{"kind":"cli","engine":"codex","account":{"mode":"user"}}"#)
                .unwrap();
        assert_eq!(
            runtime,
            AgentRuntime::Cli(CliRuntime {
                engine: "codex".into(),
                model: None,
                reasoning: None,
                account: AccountBinding::User,
            })
        );
    }

    #[test]
    fn every_malformed_runtime_is_refused_with_its_own_reason() {
        for (json, expected) in [
            ("not json", "valid JSON"),
            ("[]", "JSON object"),
            (r#"{}"#, "'kind'"),
            (r#"{"kind":"gpu"}"#, "must be 'llm' or 'cli'"),
            (r#"{"kind":"cli","account":{"mode":"user"}}"#, "'engine'"),
            (
                r#"{"kind":"cli","engine":"nano-bot","account":{"mode":"user"}}"#,
                "unknown agent engine",
            ),
            (r#"{"kind":"cli","engine":"codex"}"#, "'account'"),
            (r#"{"kind":"cli","engine":"codex","account":{}}"#, "'mode'"),
            (
                r#"{"kind":"cli","engine":"codex","account":{"mode":"global"}}"#,
                "must name the account",
            ),
            (
                r#"{"kind":"cli","engine":"codex","account":{"mode":"user","account_id":"acc-1"}}"#,
                "cannot name one",
            ),
            (
                r#"{"kind":"cli","engine":"codex","account":{"mode":"engine"}}"#,
                "'global' or 'user'",
            ),
        ] {
            let error = AgentRuntime::parse(json).expect_err(json);
            assert!(
                error.to_string().contains(expected),
                "{json}: expected '{expected}', got '{error}'"
            );
        }
    }

    #[test]
    fn a_global_binding_must_match_an_existing_account_of_the_same_engine() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        provider_accounts::repository::create_account(
            &db,
            &provider_accounts::NewAccount {
                account_id: "acc-claude".into(),
                org_id: "org-default".into(),
                engine_id: "claude-code".into(),
                display_name: "Firma".into(),
                scope: "global".into(),
                owner_user_id: None,
                credential_kind: "provider_login".into(),
                created_by: "admin".into(),
            },
        )
        .unwrap();

        let ok = AgentRuntime::parse(
            r#"{"kind":"cli","engine":"claude-code","account":{"mode":"global","account_id":"acc-claude"}}"#,
        )
        .unwrap();
        validate_account_binding(&db, "org-default", &ok).unwrap();

        let wrong_engine = AgentRuntime::parse(
            r#"{"kind":"cli","engine":"codex","account":{"mode":"global","account_id":"acc-claude"}}"#,
        )
        .unwrap();
        let error = validate_account_binding(&db, "org-default", &wrong_engine).unwrap_err();
        assert!(error.to_string().contains("runs claude-code"), "{error}");

        let missing = AgentRuntime::parse(
            r#"{"kind":"cli","engine":"codex","account":{"mode":"global","account_id":"nope"}}"#,
        )
        .unwrap();
        let error = validate_account_binding(&db, "org-default", &missing).unwrap_err();
        assert!(error.to_string().contains("does not exist"), "{error}");

        let other_org = validate_account_binding(&db, "org-other", &ok).unwrap_err();
        assert!(
            other_org.to_string().contains("another organisation"),
            "{other_org}"
        );

        // A personal account is never assignable: it answers to its owner, and
        // an agent is run by whoever may run it.
        provider_accounts::repository::create_account(
            &db,
            &provider_accounts::NewAccount {
                account_id: "acc-mine".into(),
                org_id: "org-default".into(),
                engine_id: "codex".into(),
                display_name: "Moje".into(),
                scope: "user".into(),
                owner_user_id: Some("alice".into()),
                credential_kind: "provider_login".into(),
                created_by: "alice".into(),
            },
        )
        .unwrap();
        let personal = AgentRuntime::parse(
            r#"{"kind":"cli","engine":"codex","account":{"mode":"global","account_id":"acc-mine"}}"#,
        )
        .unwrap();
        let error = validate_account_binding(&db, "org-default", &personal).unwrap_err();
        assert!(error.to_string().contains("personal account"), "{error}");

        // An LLM agent has nothing to bind.
        validate_account_binding(&db, "org-default", &AgentRuntime::Llm).unwrap();
    }
}
