// =============================================================================
// File: services/streaming/mod.rs — per-camera bounded broadcast bus
// =============================================================================
//
// Fan-out of camera frames to subscribed consumers. Each subscriber owns a
// bounded mpsc channel; on overflow the bus drops the frame and emits a
// `Drop { count }` signal before the next successful send so the consumer
// learns it fell behind.

mod bus;

pub use bus::{
    NextOutcome, StreamFilter, StreamId, StreamMessage, StreamSubscriber, StreamingBus,
    SUBSCRIBER_CAPACITY,
};
