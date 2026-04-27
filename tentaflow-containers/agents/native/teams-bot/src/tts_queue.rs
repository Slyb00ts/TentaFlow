// =============================================================================
// Plik: tts_queue.rs
// Opis: Sekwencyjna kolejka zadan TTS — kolejne zdania ida do syntezy w
//       kolejnosci pojawienia sie w streamie LLM. Zachowuje FIFO audio
//       chunkow w mikrofonie i nie obciaza backendu TTS rownoleglymi
//       requestami (sherpa-onnx jest single-threaded per session).
// =============================================================================

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, Mutex};

type BoxedTask = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Sekwencyjny executor zadan async. Workflow:
///   1. caller `enqueue(fut)` — fut wedruje do kanalu mpsc
///   2. wewnetrzny worker pobiera fut, awaituje go do konca, dopiero
///      potem siega po nastepny — kolejne zdania nie ida do TTS rownoczesnie
///   3. `wait_idle` blokuje az kolejka opustoszeje (wszystkie zdania
///      zsyntetyzowane i wypchniete do audio_playback).
pub struct TtsQueue {
    tx: mpsc::UnboundedSender<BoxedTask>,
    /// Liczba zadan in-flight. AtomicU64 zeby `enqueue` mogl byc sync —
    /// caller (delta-token callback) jest synchroniczny i nie moze awaitowac.
    pending: Arc<AtomicU64>,
    /// Notyfikacja gdy `pending` schodzi do zera. Tokio Mutex bo waiter
    /// list jest dotykana z async kontekstu (worker + `wait_idle`).
    idle_waiters: Arc<Mutex<Vec<oneshot::Sender<()>>>>,
}

impl TtsQueue {
    /// Spawnuje wewnetrzny worker i zwraca handle. Worker zyje dopoki
    /// `tx` nie zostanie dropniety razem z handle'em.
    pub fn spawn() -> Arc<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<BoxedTask>();
        let pending = Arc::new(AtomicU64::new(0));
        let idle_waiters: Arc<Mutex<Vec<oneshot::Sender<()>>>> = Arc::new(Mutex::new(Vec::new()));

        let pending_worker = Arc::clone(&pending);
        let waiters_worker = Arc::clone(&idle_waiters);
        tokio::spawn(async move {
            while let Some(task) = rx.recv().await {
                task.await;
                let prev = pending_worker.fetch_sub(1, Ordering::AcqRel);
                if prev == 1 {
                    let mut waiters = waiters_worker.lock().await;
                    for w in waiters.drain(..) {
                        let _ = w.send(());
                    }
                }
            }
        });

        Arc::new(Self {
            tx,
            pending,
            idle_waiters,
        })
    }

    /// Wrzuca nowe zadanie na koniec kolejki. Sync — wolane z FnMut
    /// callbackow (delta-token), ktore nie moga awaitowac. Worker odpali
    /// `fut` dopiero po zakonczeniu poprzedniego.
    pub fn enqueue<Fut>(&self, fut: Fut)
    where
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.pending.fetch_add(1, Ordering::AcqRel);
        if self.tx.send(Box::pin(fut)).is_err() {
            // Worker juz nie zyje — defensywnie odejmujemy licznik
            // zeby `wait_idle` nie zawisl. W praktyce nie zdarza sie
            // dopoki `Arc<TtsQueue>` zyje.
            self.pending.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Czeka az wszystkie wczesniej zaenqueueowane zadania sie skoncza.
    /// Jezeli kolejka jest juz pusta — wraca natychmiast.
    pub async fn wait_idle(&self) {
        if self.pending.load(Ordering::Acquire) == 0 {
            return;
        }
        let (tx, rx) = oneshot::channel();
        self.idle_waiters.lock().await.push(tx);
        // Re-check po zarejestrowaniu waitera — worker mogl skonczyc i
        // zwolnic waiterow w oknie miedzy load a push. Bez tego pierwszy
        // `wait_idle` po krotkim zadaniu zawisby na zawsze.
        if self.pending.load(Ordering::Acquire) == 0 {
            // Wybudz sami siebie jezeli juz pusto.
            let mut waiters = self.idle_waiters.lock().await;
            for w in waiters.drain(..) {
                let _ = w.send(());
            }
        }
        let _ = rx.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn tasks_run_sequentially() {
        let queue = TtsQueue::spawn();
        let order = Arc::new(Mutex::new(Vec::<u64>::new()));

        for i in 0..5u64 {
            let order_clone = Arc::clone(&order);
            queue.enqueue(async move {
                // Krotki sleep zeby jakikolwiek concurrent scheduling
                // ujawnil sie w teste — sekwencyjny worker zawsze daje
                // monotonicznie rosnaca kolejnosc.
                tokio::time::sleep(Duration::from_millis(5)).await;
                order_clone.lock().await.push(i);
            });
        }

        queue.wait_idle().await;
        let final_order = order.lock().await.clone();
        assert_eq!(final_order, vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn wait_idle_returns_immediately_when_empty() {
        let queue = TtsQueue::spawn();
        // Brak zadan — wait_idle nie blokuje.
        tokio::time::timeout(Duration::from_millis(50), queue.wait_idle())
            .await
            .expect("wait_idle powinno wrocic natychmiast");
    }

    #[tokio::test]
    async fn wait_idle_unblocks_after_completion() {
        let queue = TtsQueue::spawn();
        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = Arc::clone(&counter);
        queue.enqueue(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });
        queue.wait_idle().await;
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }
}
