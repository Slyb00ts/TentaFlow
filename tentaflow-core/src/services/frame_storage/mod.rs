// =============================================================================
// File: services/frame_storage/mod.rs — LRU in-memory frame buffer
// =============================================================================
//
// Per-process shared storage for raw camera frames. Each frame gets a unique
// `frame_<uuid>` ref returned to callers (Chunk C `service_call_v1`) who can
// later issue a `PickupToken`; the consuming service then fetches via the
// Service-to-Core API. F1a is single-node — frames die on process restart.

mod lru;

pub use lru::{
    FrameMetadata, FramePixelFormat, FrameStorage, FrameStorageStats, RawFrameRef, StoredFrame,
};
