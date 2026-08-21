// ===== File: services/ingest_gate.rs — one concurrency bound for document ingest =====
//
// Extraction plus embedding of several documents at once holds hundreds of MiB
// of chunk text and vectors, so ingestion needs a ceiling. Project Studio had
// one (a job-level semaphore) and the RAG addon had none — the same pipeline
// bounded on one path and unbounded on the other.
//
// The bound lives at `ModelRuntimeExecutor::execute_ingest`, the single choke
// point both callers pass through, so neither can forget it and neither can
// disagree about the limit. It counts DOCUMENTS rather than jobs, which is what
// actually bounds memory: a job is a sequence of documents, and gating the job
// let one job's document run alongside another's regardless.

use tokio::sync::{Semaphore, SemaphorePermit};

/// Documents that may be in the extract/embed pipeline at once.
pub const MAX_CONCURRENT_DOCUMENT_INGESTS: usize = 2;

fn semaphore() -> &'static Semaphore {
    static SEM: std::sync::OnceLock<Semaphore> = std::sync::OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(MAX_CONCURRENT_DOCUMENT_INGESTS))
}

/// Waits for a slot. The permit releases on drop, including on panic, so a
/// failed ingest can never leak capacity.
pub async fn acquire() -> Option<SemaphorePermit<'static>> {
    // The semaphore is a process-lifetime static and is never closed; a closed
    // error would mean the process is tearing down, and the caller should
    // proceed rather than panic on the way out.
    semaphore().acquire().await.ok()
}

/// Whether a caller would have to wait. Only a hint for progress reporting —
/// the answer can change before `acquire()` runs, so it must never gate work.
pub fn would_wait() -> bool {
    semaphore().available_permits() == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Takes the WHOLE gate for the duration of a test. The semaphore is a
    /// process-lifetime static shared by every caller in the test binary, so a
    /// gate test that did not own every permit would be measuring somebody
    /// else's ingest; permits are then handed back one at a time with `split`,
    /// so the gate is never left open for someone to walk into halfway through
    /// an assertion.
    ///
    /// The mutex is what makes that safe between two gate tests. The semaphore
    /// is FAIR: a second `acquire_many(MAX)` queued behind the first would be
    /// handed the split-off permit as part of its batch and sit on it, and the
    /// single-permit `acquire()` this test is measuring would queue behind that
    /// forever. Gate tests therefore take turns outside the semaphore.
    async fn own_the_gate() -> (
        tokio::sync::MutexGuard<'static, ()>,
        tokio::sync::SemaphorePermit<'static>,
    ) {
        static TURN: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        let turn = TURN.get_or_init(Default::default).lock().await;
        let permit = semaphore()
            .acquire_many(MAX_CONCURRENT_DOCUMENT_INGESTS as u32)
            .await
            .expect("the gate semaphore is never closed");
        (turn, permit)
    }

    /// A permit is real capacity: while the bound is reached nobody else gets
    /// in, and dropping one hands that capacity straight back.
    #[tokio::test]
    async fn the_gate_admits_exactly_the_documented_number_of_documents() {
        let (_turn, mut all) = own_the_gate().await;
        assert_eq!(semaphore().available_permits(), 0);
        assert!(would_wait(), "with the bound reached the gate reports a wait");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), acquire())
                .await
                .is_err(),
            "a document arriving at a full gate waits instead of running"
        );

        // Hand back exactly one permit, so the free capacity is ours alone.
        drop(all.split(1).expect("one permit of the batch"));
        let mine = tokio::time::timeout(std::time::Duration::from_secs(5), acquire())
            .await
            .expect("the freed permit is handed out")
            .expect("the gate is open");
        assert!(would_wait(), "the last permit is out; the bound still holds");

        drop(mine);
        let again = tokio::time::timeout(std::time::Duration::from_secs(5), acquire())
            .await
            .expect("dropping a permit returns the capacity")
            .expect("the gate is open");
        drop(again);
        drop(all);
        assert_eq!(
            semaphore().available_permits(),
            MAX_CONCURRENT_DOCUMENT_INGESTS,
            "every permit is back"
        );
    }

    /// An ingest that panics must not take its slot with it. The permit is held
    /// across the whole flow, so a leak here does not fail one document — it
    /// shrinks the gate for the life of the process, one panic at a time, until
    /// nothing ingests at all.
    #[tokio::test]
    async fn a_panicking_ingest_hands_its_permit_back() {
        let (_turn, mut all) = own_the_gate().await;
        drop(all.split(1).expect("one permit of the batch"));

        let acquired = Arc::new(AtomicBool::new(false));
        let flag = acquired.clone();
        let blown = tokio::spawn(async move {
            let _permit = acquire().await.expect("the gate is open");
            flag.store(true, Ordering::SeqCst);
            panic!("ingest blew up holding the gate");
        })
        .await;
        assert!(blown.is_err(), "the task really panicked");
        assert!(acquired.load(Ordering::SeqCst), "it really held a permit");

        let after = tokio::time::timeout(std::time::Duration::from_secs(5), acquire())
            .await
            .expect("the panicked ingest released its permit")
            .expect("the gate is open");
        drop(after);
        drop(all);
        assert_eq!(
            semaphore().available_permits(),
            MAX_CONCURRENT_DOCUMENT_INGESTS,
            "a panic leaks no capacity"
        );
    }
}
