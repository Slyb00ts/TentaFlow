// =============================================================================
// File: flow_runtime/tests/bounded_drop_oldest_tests.rs — channel primitive
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use crate::flow_runtime::bounded_drop_oldest::BoundedDropOldest;

#[tokio::test]
async fn send_below_cap_no_drop() {
    let ch = BoundedDropOldest::<u32>::new(4);
    for v in 0..3 {
        ch.send(v);
    }
    assert_eq!(ch.dropped(), 0);
    assert_eq!(ch.len(), 3);
    assert_eq!(ch.recv().await, Some(0));
    assert_eq!(ch.recv().await, Some(1));
    assert_eq!(ch.recv().await, Some(2));
}

#[tokio::test]
async fn send_above_cap_drops_oldest() {
    let ch = BoundedDropOldest::<u32>::new(3);
    for v in 0..5 {
        ch.send(v);
    }
    assert_eq!(ch.dropped(), 2);
    assert_eq!(ch.len(), 3);
    // 0 and 1 evicted; 2,3,4 remain in FIFO order.
    assert_eq!(ch.recv().await, Some(2));
    assert_eq!(ch.recv().await, Some(3));
    assert_eq!(ch.recv().await, Some(4));
}

#[tokio::test]
async fn close_terminates_recv() {
    let ch = BoundedDropOldest::<u32>::new(4);
    ch.send(7);
    ch.close();
    assert_eq!(ch.recv().await, Some(7));
    assert_eq!(ch.recv().await, None);
    // Idempotent close.
    ch.close();
    assert_eq!(ch.recv().await, None);
}

#[tokio::test]
async fn recv_blocks_until_send() {
    let ch = BoundedDropOldest::<u32>::new(2);
    let ch2 = ch.clone();
    let join = tokio::spawn(async move { ch2.recv().await });
    // Give the receiver time to park on the empty buffer.
    tokio::time::sleep(Duration::from_millis(20)).await;
    ch.send(42);
    let got = tokio::time::timeout(Duration::from_secs(1), join)
        .await
        .expect("recv woke up")
        .expect("join")
        .expect("Some");
    assert_eq!(got, 42);
}

#[tokio::test]
async fn concurrent_send_recv_safe() {
    let ch = BoundedDropOldest::<u32>::new(8);
    let producer = {
        let ch = ch.clone();
        tokio::spawn(async move {
            for v in 0..200 {
                ch.send(v);
                if v % 7 == 0 {
                    tokio::task::yield_now().await;
                }
            }
            ch.close();
        })
    };
    let consumer = {
        let ch: Arc<BoundedDropOldest<u32>> = ch.clone();
        tokio::spawn(async move {
            let mut count = 0u32;
            while ch.recv().await.is_some() {
                count += 1;
            }
            count
        })
    };
    producer.await.expect("producer joined");
    let count = consumer.await.expect("consumer joined");
    // Some sends raced past the cap of 8 and were dropped. Verify the
    // received count plus drops equals the produced total.
    assert_eq!(count as u64 + ch.dropped(), 200);
}
