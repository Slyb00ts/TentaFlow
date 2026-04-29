// ============ File: services/lifecycle.rs — service lifecycle state types ============

use std::time::Duration;

use crate::services::transport::Transport;
use crate::services_repo::services::{DeployMethod, ServiceStatus};

/// Identity of a service inside the running node — combines DB id with the
/// engine identifier from the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServiceHandle {
    pub id: i64,
    pub engine_id: String,
}

/// Snapshot of where a service runs and how to reach it. Built once after a
/// successful deploy and cached by the registry.
#[derive(Debug, Clone)]
pub struct ServiceEndpoint {
    pub handle: ServiceHandle,
    pub transport: Transport,
    pub deploy_method: DeployMethod,
    pub status: ServiceStatus,
    pub host: String,
    pub runtime_port: Option<u16>,
    pub sidecar_quic_port: Option<u16>,
    pub url: Option<String>,
}

/// Reason a supervisor decided to restart a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartReason {
    HealthCheckFailed,
    ProcessExited,
    ManualRequest,
}

/// Backoff schedule applied between restart attempts. Exponential with a cap.
#[derive(Debug, Clone, Copy)]
pub struct RestartPolicy {
    pub max_attempts: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl RestartPolicy {
    pub fn default_policy() -> Self {
        Self {
            max_attempts: 5,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        }
    }

    /// Computes the delay for attempt `n` (1-indexed) by doubling the initial
    /// delay until `max_delay` is reached.
    pub fn delay_for_attempt(self, n: u32) -> Duration {
        if n == 0 {
            return self.initial_delay;
        }
        let exponent = n.saturating_sub(1).min(20);
        let factor = 1u64 << exponent;
        let scaled = self.initial_delay.saturating_mul(factor as u32);
        scaled.min(self.max_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_delay_grows_then_caps() {
        let p = RestartPolicy {
            max_attempts: 10,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(8),
        };
        assert_eq!(p.delay_for_attempt(1), Duration::from_secs(1));
        assert_eq!(p.delay_for_attempt(2), Duration::from_secs(2));
        assert_eq!(p.delay_for_attempt(3), Duration::from_secs(4));
        assert_eq!(p.delay_for_attempt(4), Duration::from_secs(8));
        // capped at max_delay
        assert_eq!(p.delay_for_attempt(20), Duration::from_secs(8));
    }
}
