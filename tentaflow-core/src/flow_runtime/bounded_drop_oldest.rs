// =============================================================================
// File: flow_runtime/bounded_drop_oldest.rs — drop-oldest bounded channel
// =============================================================================
//
// `tokio::sync::mpsc` does not expose a drop-oldest semantic: once `try_send`
// returns `Full` the sender cannot atomically pop the oldest receiver-side
// item and push a fresh one. Crossbeam offers similar drop options but its
// blocking `recv` does not integrate with tokio's runtime without bridging
// threads.
//
// `BoundedDropOldest<T>` is the minimal primitive the scheduler needs:
//
//   * `send` is sync, non-blocking, always succeeds. If the buffer is full it
//     evicts the OLDEST item (`pop_front`) before pushing the new value
//     (`push_back`). The eviction count is exposed via `dropped()` so a
//     per-invocation finalize audit can surface backpressure pressure.
//   * `recv` is async, awakened by `Notify` on every send (and on `close`).
//   * `close` is a one-shot terminator: subsequent `recv` returns `None` once
//     the buffer is drained. A closed channel still accepts `send` but
//     re-closing is a no-op (idempotent).
//
// The internal `VecDeque` is guarded by `parking_lot::Mutex` to avoid the std
// lock poisoning footgun; under contention the critical section is a single
// push/pop so the lock is held for tens of nanoseconds.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

pub struct BoundedDropOldest<T> {
    buf: Mutex<VecDeque<T>>,
    notify: Notify,
    cap: usize,
    closed: AtomicBool,
    dropped: AtomicU64,
}

impl<T> BoundedDropOldest<T> {
    pub fn new(cap: usize) -> Arc<Self> {
        assert!(cap > 0, "BoundedDropOldest capacity must be > 0");
        Arc::new(Self {
            buf: Mutex::new(VecDeque::with_capacity(cap)),
            notify: Notify::new(),
            cap,
            closed: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
        })
    }

    /// Push `v`. When the buffer is already at capacity the oldest item is
    /// evicted (`pop_front`) and `dropped()` is incremented by one. Always
    /// notifies one waiter so a receiver parked on an empty buffer wakes up.
    pub fn send(&self, v: T) {
        {
            let mut g = self.buf.lock();
            if g.len() >= self.cap {
                g.pop_front();
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            g.push_back(v);
        }
        self.notify.notify_one();
    }

    /// Awaits the next item. Returns `None` once the channel is closed AND
    /// the buffer is drained. The order of the two checks matters: if a
    /// sender pushes the final item and then closes, the receiver must still
    /// observe that item before the `None` terminator.
    pub async fn recv(&self) -> Option<T> {
        loop {
            // Register interest BEFORE we sample the queue so a concurrent
            // `send` between the empty-check and `notified().await` cannot be
            // missed (tokio Notify is permit-based).
            let notified = self.notify.notified();
            {
                let mut g = self.buf.lock();
                if let Some(v) = g.pop_front() {
                    return Some(v);
                }
                if self.closed.load(Ordering::Acquire) {
                    return None;
                }
            }
            notified.await;
        }
    }

    /// Marks the channel closed. Subsequent `recv` calls drain whatever is
    /// still buffered then return `None`. Idempotent.
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            // Wake every parked receiver so they re-check the closed flag.
            self.notify.notify_waiters();
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.buf.lock().len()
    }
}
