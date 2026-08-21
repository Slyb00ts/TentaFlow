// ===== File: agents/principal.rs — AgentPrincipal: the identity a harness run
// acts under. Threads user_id/org_id into addon tool permission checks
// (Harness §3.3). A run ALWAYS has a principal; `user_id == None` is the
// unattended-call case where addon tools are denied (core.* still work). =====

use crate::db::models::DbAgentRun;
use crate::flow_engine::dispatcher::{ActorKind, FlowActor, FlowOrigin};

/// Principal of an agent run. `user_id` gates every addon tool call through the
/// addon permission engine; `org_id` attributes the run for compliance audit.
/// Children (spawned agents) inherit the parent principal verbatim (§3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPrincipal {
    /// Owning user. `None` = unattended (external call without a session):
    /// addon tools are denied (permission_checker has no user to grant for),
    /// only core.* builtins run.
    pub user_id: Option<String>,
    /// Owning organization for compliance attribution (multi-tenant).
    pub org_id: Option<String>,
    /// §2.5 — provenance of the run: where the request that started this agent
    /// entered the system. A child agent inherits it verbatim, so a sub-agent
    /// three levels down still reports the Code Studio / chat / API entry that
    /// set the whole chain in motion.
    pub origin: FlowOrigin,
    /// §2.5 — the authenticated caller behind the run, inherited by children
    /// alongside `origin`. An API key stays an API key: nothing derives this
    /// from `user_id`, because that would report a service key as a user.
    pub actor: FlowActor,
    /// §2.5 — ties every run in the chain to the audit / compliance trail of
    /// the turn that started it. Inherited by children like the rest.
    pub correlation_id: Option<String>,
}

impl AgentPrincipal {
    /// §2.5: `origin` and `actor` are mandatory positional arguments — there is
    /// no defaulted variant and no optional stamping step, so a caller that
    /// does not say where a run came from fails to COMPILE rather than
    /// reporting an invented `agent` origin with an actor guessed from
    /// `user_id`. `correlation_id` starts empty; a caller holding the turn's
    /// audit key sets it with [`AgentPrincipal::with_correlation_id`].
    pub fn new(
        user_id: Option<String>,
        org_id: Option<String>,
        origin: FlowOrigin,
        actor: FlowActor,
    ) -> Self {
        Self {
            user_id,
            org_id,
            origin,
            actor,
            correlation_id: None,
        }
    }

    /// A run started directly by a user with no outer entry point behind it:
    /// origin `agent`, the user as the actor. Both values are stated by this
    /// constructor's contract, not defaulted for a caller who forgot them.
    pub fn user(user_id: impl Into<String>) -> Self {
        let user_id = user_id.into();
        Self {
            actor: FlowActor::user(user_id.clone()),
            user_id: Some(user_id),
            org_id: None,
            origin: FlowOrigin::Agent,
            correlation_id: None,
        }
    }

    /// Rebuilds the principal a persisted run acted under, from its row.
    ///
    /// This is a READ, not a derivation: `actor_kind` / `actor_id` /
    /// `actor_user_id` come back exactly as they were written, so an API key
    /// stays an API key. `None` when `origin` or `actor_kind` is not a spelling
    /// [`FlowOrigin::as_str`] / [`ActorKind::as_str`] produces — an unreadable
    /// stamp must REFUSE — including a run written BEFORE migration v134, whose
    /// columns are NULL. Falling back to "a user named by `user_id`" is how a
    /// service key gets reported as a person, and a fallback chain here would be
    /// invisible at exactly the moment it lies.
    pub fn from_run_row(run: &DbAgentRun) -> Option<Self> {
        let origin = FlowOrigin::parse(run.origin.as_deref()?)?;
        let actor_kind = ActorKind::parse(run.actor_kind.as_deref()?)?;
        Some(Self {
            user_id: run.user_id.clone(),
            org_id: run.org_id.clone(),
            origin,
            actor: FlowActor::from_parts(
                actor_kind,
                run.actor_id.clone(),
                run.actor_user_id.clone(),
            ),
            correlation_id: run.correlation_id.clone(),
        })
    }

    /// Ties the run to the audit trail of the turn that started it.
    pub fn with_correlation_id(mut self, correlation_id: Option<String>) -> Self {
        self.correlation_id = correlation_id;
        self
    }

    /// `Some(user_id)` when addon tool permission checks are possible. Used by
    /// the tool catalog to skip addon tools entirely for unattended runs.
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{AgentParams, NewAgentRun};
    use crate::db::repository;
    use std::path::Path;

    fn db_with_agent(agent_id: &str) -> crate::db::DbPool {
        let pool = crate::db::init(Path::new(":memory:")).expect("in-memory db");
        repository::upsert_agent(
            &pool,
            &AgentParams {
                id: agent_id,
                name: agent_id,
                display_name: Some("Runner"),
                description: "provenance round-trip fixture",
                system_prompt: None,
                model: None,
                tools_json: "[]",
                skills_json: "{}",
                params_json: "{}",
                max_iterations: 3,
                timeout_secs: 30,
                max_subagents: 2,
                max_spawn_depth: 2,
                flow_id: None,
                routable: false,
                is_enabled: true,
                on_child_complete: "notify",
                allowed_agents_json: None,
                actor_user_id: None,
            },
        )
        .expect("agent");
        pool
    }

    /// §2.5 — a run started by a SERVICE API key must come back off its row as
    /// an API key with no user behind it. The old test cloned a principal inside
    /// the test body, which proved only that `Clone` copies fields; this drives
    /// the real path a continuation uses: `create_agent_run` writes the stamp,
    /// `get_agent_run` reads the row, `from_run_row` rebuilds the principal.
    #[test]
    fn principal_round_trips_an_api_key_run_without_becoming_a_user() {
        let pool = db_with_agent("runner");
        let parent = AgentPrincipal::new(
            None,
            Some("org-1".into()),
            FlowOrigin::Api,
            FlowActor::api_key("key-77", None),
        )
        .with_correlation_id(Some("corr-9".into()));

        repository::create_agent_run(
            &pool,
            &NewAgentRun {
                id: "run-1",
                agent_id: "runner",
                parent_run_id: None,
                flow_execution_id: None,
                user_id: parent.user_id(),
                org_id: parent.org_id.as_deref(),
                prompt: "do it",
                origin: parent.origin.as_str(),
                actor_kind: parent.actor.kind().as_str(),
                actor_id: parent.actor.id(),
                actor_user_id: parent.actor.user_id(),
                correlation_id: parent.correlation_id.as_deref(),
            },
        )
        .expect("create run");

        let row = repository::get_agent_run(&pool, "run-1")
            .expect("read run")
            .expect("run exists");
        let child = AgentPrincipal::from_run_row(&row).expect("readable provenance");

        assert_eq!(child, parent);
        assert_eq!(child.actor.kind(), ActorKind::ApiKey);
        assert_eq!(child.actor.id(), Some("key-77"));
        // The service key has no user; deriving one from `user_id` would report
        // this run as a person's.
        assert_eq!(child.actor.user_id(), None);
        assert_eq!(child.origin, FlowOrigin::Api);
    }

    /// The dangerous shape: a key BOUND to a user. `user_id` is set (org / ACL
    /// attribution), so any code that rebuilds the actor from `user_id` produces
    /// a plausible-looking `FlowActor::user(...)` and the call stops being
    /// recognisable as an API key. The row is the authority, not `user_id`.
    #[test]
    fn user_bound_api_key_run_stays_an_api_key_not_the_user_it_is_bound_to() {
        let pool = db_with_agent("runner");
        repository::create_agent_run(
            &pool,
            &NewAgentRun {
                id: "run-bound",
                agent_id: "runner",
                parent_run_id: None,
                flow_execution_id: None,
                // Set exactly as the /v1 path sets it for a user-bound key.
                user_id: Some("u-5"),
                org_id: Some("org-1"),
                prompt: "do it",
                origin: FlowOrigin::Api.as_str(),
                actor_kind: ActorKind::ApiKey.as_str(),
                actor_id: Some("key-88"),
                actor_user_id: Some("u-5"),
                correlation_id: None,
            },
        )
        .expect("create run");

        let row = repository::get_agent_run(&pool, "run-bound")
            .expect("read run")
            .expect("run exists");
        let principal = AgentPrincipal::from_run_row(&row).expect("readable provenance");

        assert_eq!(principal.user_id.as_deref(), Some("u-5"));
        assert_eq!(principal.actor.kind(), ActorKind::ApiKey);
        assert_eq!(principal.actor.id(), Some("key-88"));
        assert_eq!(principal.actor.user_id(), Some("u-5"));
        assert_ne!(principal.actor, FlowActor::user("u-5"));
    }

    /// A Code Studio run keeps its entry point across the round trip, so a
    /// continuation three levels down still reports where the chain started.
    #[test]
    fn code_studio_origin_survives_the_row() {
        let pool = db_with_agent("runner");
        repository::create_agent_run(
            &pool,
            &NewAgentRun {
                id: "run-2",
                agent_id: "runner",
                parent_run_id: None,
                flow_execution_id: None,
                user_id: Some("u-1"),
                org_id: Some("org-1"),
                prompt: "assist",
                origin: FlowOrigin::CodeStudio.as_str(),
                actor_kind: ActorKind::User.as_str(),
                actor_id: Some("u-1"),
                actor_user_id: Some("u-1"),
                correlation_id: Some("corr-2"),
            },
        )
        .expect("create run");

        let row = repository::get_agent_run(&pool, "run-2")
            .expect("read run")
            .expect("run exists");
        let principal = AgentPrincipal::from_run_row(&row).expect("readable provenance");
        assert_eq!(principal.origin, FlowOrigin::CodeStudio);
        assert_eq!(principal.actor, FlowActor::user("u-1"));
        assert_eq!(principal.correlation_id.as_deref(), Some("corr-2"));
    }

    /// §3 invariant 1 / rule 2 (no fallback chains): a stamp we cannot read is a
    /// REFUSAL, not a default. Returning `system` (or a user derived from
    /// `user_id`) here would be a lie the event log then keeps forever.
    #[test]
    fn unreadable_stamp_refuses_instead_of_defaulting() {
        let pool = db_with_agent("runner");
        repository::create_agent_run(
            &pool,
            &NewAgentRun {
                id: "run-3",
                agent_id: "runner",
                parent_run_id: None,
                flow_execution_id: None,
                user_id: Some("u-1"),
                org_id: None,
                prompt: "x",
                origin: "from_the_future",
                actor_kind: ActorKind::User.as_str(),
                actor_id: Some("u-1"),
                actor_user_id: Some("u-1"),
                correlation_id: None,
            },
        )
        .expect("create run");
        let row = repository::get_agent_run(&pool, "run-3")
            .expect("read run")
            .expect("run exists");
        assert!(AgentPrincipal::from_run_row(&row).is_none());

        repository::create_agent_run(
            &pool,
            &NewAgentRun {
                id: "run-4",
                agent_id: "runner",
                parent_run_id: None,
                flow_execution_id: None,
                user_id: Some("u-1"),
                org_id: None,
                prompt: "x",
                origin: FlowOrigin::Chat.as_str(),
                actor_kind: "robot",
                actor_id: Some("u-1"),
                actor_user_id: None,
                correlation_id: None,
            },
        )
        .expect("create run");
        let row = repository::get_agent_run(&pool, "run-4")
            .expect("read run")
            .expect("run exists");
        assert!(AgentPrincipal::from_run_row(&row).is_none());
    }

    /// `FlowOrigin::parse` / `ActorKind::parse` are exact inverses of `as_str`
    /// — every variant survives a write/read cycle, so a variant without its
    /// parse arm is caught here rather than silently becoming unreadable rows.
    ///
    /// The variant lists come from `all_flow_origins` / `all_actor_kinds`, which
    /// the compiler keeps complete. The hand-written lists that used to stand
    /// here had the same failure mode as the bug they were meant to catch: both
    /// omitted `Dashboard` and `Meeting`, so deleting `"meeting"` from `parse`
    /// left this test green while every meeting-bot run row became unreadable.
    #[test]
    fn parse_is_the_exact_inverse_of_as_str() {
        for origin in crate::flow_engine::dispatcher::all_flow_origins() {
            assert_eq!(FlowOrigin::parse(origin.as_str()), Some(origin));
        }
        for kind in crate::flow_engine::dispatcher::all_actor_kinds() {
            assert_eq!(ActorKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(FlowOrigin::parse(""), None);
        assert_eq!(FlowOrigin::parse("Chat"), None);
        assert_eq!(ActorKind::parse("apikey"), None);
    }

    /// `AgentPrincipal::user` is the "no outer entry point" constructor; it
    /// states `agent` + the user, and the unattended case has no actor at all.
    #[test]
    fn user_constructor_states_agent_origin_and_unattended_has_no_actor() {
        let p = AgentPrincipal::user("u-9");
        assert_eq!(p.origin, FlowOrigin::Agent);
        assert_eq!(p.actor, FlowActor::user("u-9"));

        let unattended = AgentPrincipal::new(None, None, FlowOrigin::System, FlowActor::system());
        assert_eq!(unattended.actor, FlowActor::system());
        assert!(unattended.user_id().is_none());
    }

    /// A run written BEFORE migration v134 has NULL provenance columns. That is
    /// the honest "predates the stamp", and it must refuse the same way a
    /// corrupt value does — a continuation of such a run would otherwise be
    /// launched under a principal nobody ever observed.
    #[test]
    fn pre_migration_row_without_a_stamp_refuses() {
        let pool = db_with_agent("runner");
        repository::create_agent_run(
            &pool,
            &NewAgentRun {
                id: "run-5",
                agent_id: "runner",
                parent_run_id: None,
                flow_execution_id: None,
                user_id: Some("u-1"),
                org_id: None,
                prompt: "x",
                origin: FlowOrigin::Chat.as_str(),
                actor_kind: ActorKind::User.as_str(),
                actor_id: Some("u-1"),
                actor_user_id: Some("u-1"),
                correlation_id: None,
            },
        )
        .expect("create run");
        // Simulate the pre-migration shape the ALTER TABLE leaves behind.
        {
            let conn = pool.write().expect("db lock");
            conn.execute(
                "UPDATE agent_runs SET origin = NULL, actor_kind = NULL, \
                 actor_id = NULL, actor_user_id = NULL WHERE id = 'run-5'",
                [],
            )
            .expect("null out");
        }
        let row = repository::get_agent_run(&pool, "run-5")
            .expect("row still readable")
            .expect("run exists");
        assert!(row.origin.is_none());
        assert!(AgentPrincipal::from_run_row(&row).is_none());
    }
}
