// =============================================================================
// Plik: flow_engine/cancel_on_drop.rs
// Opis: Stream wrapper that fires a CancellationToken when dropped — but only
//       when the inner stream did NOT reach EOF. Used in routing/streaming.rs
//       to detect client disconnects and propagate cancel into the executor
//       finalizer task. Normal collect-to-end MUST NOT cancel; only mid-stream
//       drop counts as disconnect.
// =============================================================================

use futures::Stream;
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_util::sync::CancellationToken;

pin_project! {
    pub struct CancelOnDropStream<S> {
        #[pin]
        inner: S,
        cancel: Option<CancellationToken>,
        eof_seen: bool,
    }

    impl<S> PinnedDrop for CancelOnDropStream<S> {
        fn drop(this: Pin<&mut Self>) {
            let this = this.project();
            // Cancel only on disconnect (drop before EOF). After normal EOF the
            // executor finalizer already built outcome with FinishReason::Stop;
            // firing cancel here would be a misleading signal.
            if !*this.eof_seen {
                if let Some(c) = this.cancel.take() {
                    c.cancel();
                }
            }
        }
    }
}

impl<S> CancelOnDropStream<S> {
    pub fn new(inner: S, cancel: CancellationToken) -> Self {
        Self {
            inner,
            cancel: Some(cancel),
            eof_seen: false,
        }
    }
}

impl<S: Stream> Stream for CancelOnDropStream<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        // Fuse: after EOF we never touch inner again. Some streams panic on
        // repoll past Ready(None); we promise stable Ready(None) regardless.
        if *this.eof_seen {
            return Poll::Ready(None);
        }
        match this.inner.poll_next(cx) {
            Poll::Ready(None) => {
                *this.eof_seen = true;
                Poll::Ready(None)
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::{self, StreamExt};

    #[tokio::test]
    async fn no_cancel_after_normal_eof() {
        let cancel = CancellationToken::new();
        let s = stream::iter(vec![1, 2, 3]);
        let wrapped = CancelOnDropStream::new(s, cancel.clone());
        let collected: Vec<i32> = wrapped.collect().await;
        assert_eq!(collected, vec![1, 2, 3]);
        // EOF reached → drop must NOT cancel.
        assert!(!cancel.is_cancelled());
    }

    #[tokio::test]
    async fn cancel_fires_when_dropped_mid_stream() {
        let cancel = CancellationToken::new();
        let s = stream::iter(vec![1u8; 1000]);
        {
            let mut wrapped = Box::pin(CancelOnDropStream::new(s, cancel.clone()));
            // Read one item then drop the wrapper before draining.
            let _ = wrapped.next().await;
        }
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn cancel_idempotent_when_already_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let s = stream::iter(vec![1, 2]);
        let wrapped = CancelOnDropStream::new(s, cancel.clone());
        drop(wrapped);
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn fused_after_eof_does_not_repoll_inner() {
        use std::pin::Pin;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        use std::task::{Context, Poll};

        // Stream which returns one item then panics if polled past Ready(None).
        struct PanickingAfterEof {
            polls: Arc<AtomicU32>,
            yielded: bool,
        }
        impl Stream for PanickingAfterEof {
            type Item = u32;
            fn poll_next(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Self::Item>> {
                let n = self.polls.fetch_add(1, Ordering::SeqCst);
                if !self.yielded {
                    self.yielded = true;
                    return Poll::Ready(Some(7));
                }
                if n >= 2 {
                    panic!("inner stream re-polled past EOF");
                }
                Poll::Ready(None)
            }
        }

        let cancel = CancellationToken::new();
        let counter = Arc::new(AtomicU32::new(0));
        let inner = PanickingAfterEof {
            polls: counter.clone(),
            yielded: false,
        };
        let mut wrapped = Box::pin(CancelOnDropStream::new(inner, cancel.clone()));
        assert_eq!(wrapped.next().await, Some(7));
        assert_eq!(wrapped.next().await, None);
        // Extra poll past EOF — wrapper must short-circuit, NOT delegate.
        assert_eq!(wrapped.next().await, None);
        assert_eq!(wrapped.next().await, None);
    }

    #[tokio::test]
    async fn works_with_non_unpin_stream() {
        // async_stream produces a !Unpin stream — pin_project must handle it.
        let cancel = CancellationToken::new();
        let s = async_stream::stream! {
            yield 1u32;
            yield 2u32;
        };
        let mut wrapped = Box::pin(CancelOnDropStream::new(s, cancel.clone()));
        let mut out = Vec::new();
        while let Some(x) = wrapped.next().await {
            out.push(x);
        }
        assert_eq!(out, vec![1, 2]);
        drop(wrapped);
        assert!(!cancel.is_cancelled());
    }
}
