// =============================================================================
// File: auth/actor.rs — the kind of authenticated caller behind a request
// =============================================================================
//
// One enum for every subsystem that has to tell callers apart: the flow engine
// (provenance, event log), TentaBus (topic ACL and data-hiding rules, via
// `bus::BusCallContext::actor_kind`) and the generic resource ACL
// (`db::repository::resource_permissions::check_action`). The kind is decided
// by the entry point that authenticated the caller, never read back from
// request content or from a free-text origin string.
// =============================================================================

/// Kind of authenticated caller behind a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    User,
    ApiKey,
    Addon,
    System,
}

impl ActorKind {
    /// Stable wire spelling — persisted in the event log and rendered by the
    /// UI. It is also the `subject_type` rows about this kind of caller carry
    /// in `resource_permissions` and `bus_field_policies`; no row can name a
    /// `system` subject, so a system caller only ever meets wildcard rows.
    pub fn as_str(self) -> &'static str {
        match self {
            ActorKind::User => "user",
            ActorKind::ApiKey => "api_key",
            ActorKind::Addon => "addon",
            ActorKind::System => "system",
        }
    }

    /// Exact inverse of [`ActorKind::as_str`]. `None` for anything else —
    /// reading an unknown actor kind as `System` would turn an API key into
    /// unattended core work.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "user" => ActorKind::User,
            "api_key" => ActorKind::ApiKey,
            "addon" => ActorKind::Addon,
            "system" => ActorKind::System,
            _ => return None,
        })
    }
}
