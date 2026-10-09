// ===== File: addon/background.rs — an addon's own tool, run off the caller's thread =====
//
// A service addon's tick is one blocking call into its WASM instance; anything
// slow inside it (reaching a robot that does not answer: up to a minute of
// network timeouts) stalls every later tick — telemetry, sensors, watchdogs.
// An addon hands such work to the host instead: the host runs one of the
// addon's OWN tools on a regular worker instance and the tick returns at once.
//
// One run per (addon, tool) at a time: a tick that asks again while the previous
// run is still going gets `AlreadyRunning`, so a stuck tool cannot pile up runs.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;
use tracing::warn;

use super::AddonManager;

static MANAGER: OnceLock<Weak<AddonManager>> = OnceLock::new();

fn running() -> &'static Mutex<HashSet<(String, String)>> {
    static RUNNING: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    RUNNING.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Lets addons start background runs. Called once, where the process creates its
/// manager; a weak reference, so shutdown is not held up by it.
pub fn set_manager(manager: &Arc<AddonManager>) {
    let _ = MANAGER.set(Arc::downgrade(manager));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spawn {
    Started,
    /// The same tool of this addon is still running from an earlier request.
    AlreadyRunning,
    /// No manager or no async runtime in this process (tests, tools).
    Unavailable,
}

/// Clears the in-flight mark when the run ends, also when the tool panics.
struct RunMark((String, String));

impl Drop for RunMark {
    fn drop(&mut self) {
        running().lock().remove(&self.0);
    }
}

/// Runs `tool` of `addon_id` as a system call on a blocking-pool thread.
pub fn run_own_tool(addon_id: &str, tool: &str, params: serde_json::Value) -> Spawn {
    let Some(manager) = MANAGER.get().and_then(Weak::upgrade) else {
        return Spawn::Unavailable;
    };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return Spawn::Unavailable;
    };
    let key = (addon_id.to_string(), tool.to_string());
    if !running().lock().insert(key.clone()) {
        return Spawn::AlreadyRunning;
    }
    let mark = RunMark(key);
    runtime.spawn_blocking(move || {
        let (addon_id, tool) = &mark.0;
        if let Err(e) = manager.call_tool_system(addon_id, tool, params) {
            warn!("[addon] background run of '{addon_id}.{tool}' failed: {e:#}");
        }
        drop(mark);
    });
    Spawn::Started
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_still_running_is_not_started_twice_and_frees_its_slot_when_done() {
        let key = ("addon-x".to_string(), "x.slow".to_string());
        assert!(running().lock().insert(key.clone()));
        assert!(!running().lock().insert(key.clone()), "second run refused");
        drop(RunMark(key.clone()));
        assert!(running().lock().insert(key.clone()), "slot free after the run ended");
        running().lock().remove(&key);
    }

    #[test]
    fn without_a_registered_manager_nothing_runs() {
        assert_eq!(run_own_tool("addon-y", "y.tool", serde_json::Value::Null), Spawn::Unavailable);
    }
}
