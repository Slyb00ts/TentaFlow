// ===== File: services/vision_worker/mod.rs — core-side vision worker fleet (link + supervisor) =====
//
// docs/VISION_WORKER_SHARDING.md. `link` is the UDS wire shared by the core
// (server side) and the worker process (client side, see the crate-root
// `vision_worker` module); `supervisor` owns spawn / health / respawn /
// group-kill for the whole fleet; `fleet` is the Stage-B camera assignment
// authority + link-frame router; `source` is the core-side StreamHub source
// republishing worker camera video for dashboard tiles.

pub mod fleet;
pub mod link;
pub mod source;
pub mod supervisor;
