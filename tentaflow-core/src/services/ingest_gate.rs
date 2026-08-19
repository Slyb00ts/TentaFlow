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
