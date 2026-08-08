// ===== File: model/verify_txn.rs — transakcja weryfikacji draftu =====
//
// Verify liczy drafta sciezka prefillu, wiec zostawia po sobie strony KV, wpis
// tabeli stron i zapis tokenow pod darowizne prefiksu. Zaakceptowany jest tylko
// prefiks drafta, a reszta ma zniknac bez sladu — i to jest jedyne miejsce,
// ktore o tym wie.
use super::*;

/// Ile tokenów sekwencja miała zapisane, zanim verify tknął jej bufory.
#[derive(Clone, Copy)]
pub(crate) struct RecordedTokens {
    tokens: usize,
    prefilled: usize,
}

impl RecordedTokens {
    pub(crate) fn of(seq: &SeqKv) -> Self {
        Self {
            tokens: seq.tokens.len(),
            prefilled: seq.prefilled_len,
        }
    }
}

/// Zamyka weryfikację draftu: cofa KV do zaakceptowanej długości, unieważnia
/// tabelę stron i cofa zapis tokenów.
///
/// Verify jedzie ścieżką prefillu, a ta zapisuje `tokens`/`prefilled_len` pod
/// darowiznę prefiksu — tyle że draft nie jest promptem i zaraz zniknie. Bez
/// tego cofnięcia darowizna sięgała po strony, których sekwencja nie miała.
pub(crate) fn finish_greedy_verification(
    kv: &mut KvCache,
    page_table_seq: &mut u64,
    seq: &mut SeqKv,
    base: usize,
    recorded: RecordedTokens,
    result: Result<(usize, u32)>,
) -> Result<(usize, u32)> {
    let result = match result {
        Ok((accepted, correction)) => {
            let target_len = accepted
                .checked_add(1)
                .and_then(|retained| base.checked_add(retained));
            match target_len {
                Some(target_len) if target_len <= seq.len => {
                    kv.rollback(seq, target_len);
                    Ok((accepted, correction))
                }
                _ => {
                    if seq.len >= base {
                        kv.rollback(seq, base);
                    }
                    Err(ForgeError::Scheduler(
                        "invalid speculative verification rollback target".into(),
                    ))
                }
            }
        }
        Err(error) => {
            if seq.len >= base {
                kv.rollback(seq, base);
            }
            Err(error)
        }
    };
    seq.tokens.truncate(recorded.tokens);
    seq.prefilled_len = recorded.prefilled;
    *page_table_seq = 0;
    result
}
