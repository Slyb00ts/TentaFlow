// ===== File: speculation/tests.rs — unit + property tests for the speculation framework =====

use super::cascade::{CascadeComposer, DraftSegment};
use super::ngram::NgramProposer;
use super::state::SpeculativeState;
use super::stats::ProposerStats;
use super::{verify_greedy, Proposer, SeqContext};

/// Deterministic stand-in for a draft-model/MTP proposer: always proposes a
/// fixed token run regardless of context.
struct FixedProposer {
    tokens: Vec<u32>,
}

impl Proposer for FixedProposer {
    fn propose(&mut self, _ctx: &SeqContext<'_>, budget: usize) -> Vec<u32> {
        self.tokens.iter().copied().take(budget).collect()
    }

    fn accept_feedback(&mut self, _proposed: usize, _accepted: usize) {}

    fn name(&self) -> &str {
        "fixed"
    }
}

/// Small deterministic PRNG (splitmix-style) so property tests need no deps.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn observe_all(p: &mut NgramProposer, tokens: &[u32]) {
    for &t in tokens {
        p.observe(t);
    }
}

#[test]
fn verify_greedy_prefix_lengths() {
    assert_eq!(verify_greedy(&[], &[]), 0);
    assert_eq!(verify_greedy(&[1, 2, 3], &[1, 2, 3]), 3);
    assert_eq!(verify_greedy(&[1, 2, 3], &[1, 9, 3]), 1);
    assert_eq!(verify_greedy(&[1, 2, 3], &[9, 2, 3]), 0);
    assert_eq!(verify_greedy(&[1, 2, 3], &[1, 2]), 2);
    assert_eq!(verify_greedy(&[1, 2], &[1, 2, 3, 4]), 2);
}

#[test]
fn ngram_repetitive_stream_yields_full_budget_draft() {
    // Code-like stream: the same 8-token statement repeated.
    let pattern: Vec<u32> = vec![10, 20, 30, 40, 50, 60, 70, 80];
    let mut history = Vec::new();
    for _ in 0..10 {
        history.extend_from_slice(&pattern);
    }
    let mut p = NgramProposer::new();
    observe_all(&mut p, &history);

    let draft = p.propose(&SeqContext::new(&history), 8);
    assert_eq!(draft, pattern, "draft must continue the repeating pattern");
}

#[test]
fn ngram_non_repetitive_stream_yields_empty_draft() {
    let history: Vec<u32> = (0..200).collect();
    let mut p = NgramProposer::new();
    observe_all(&mut p, &history);

    let draft = p.propose(&SeqContext::new(&history), 8);
    assert!(draft.is_empty(), "no recurrence → nothing to propose");
}

#[test]
fn ngram_longest_gram_wins_over_shorter_noise() {
    // 1-grams of token 7 recur with different followers, but the exact
    // 4-gram [1,2,3,7] recurs once with follower 42.
    let history: Vec<u32> = vec![7, 5, 7, 6, 1, 2, 3, 7, 42, 99, 1, 2, 3, 7];
    let mut p = NgramProposer::new();
    observe_all(&mut p, &history);

    let draft = p.propose(&SeqContext::new(&history), 2);
    assert_eq!(draft, vec![42, 99]);
}

#[test]
fn ngram_extends_context_beyond_observed_history() {
    // Cascade case: ctx carries draft tokens the index has never observed;
    // the lookup must still match the ctx suffix against observed history.
    let pattern: Vec<u32> = vec![10, 20, 30, 40, 50, 60, 70, 80];
    let mut history = Vec::new();
    for _ in 0..4 {
        history.extend_from_slice(&pattern);
    }
    let mut p = NgramProposer::new();
    observe_all(&mut p, &history);

    let mut ctx = history.clone();
    ctx.extend_from_slice(&[10, 20]); // unverified draft core
    let draft = p.propose(&SeqContext::new(&ctx), 4);
    assert_eq!(draft, vec![30, 40, 50, 60]);
}

#[test]
fn cascade_ngram_extends_model_draft() {
    let pattern: Vec<u32> = vec![10, 20, 30, 40, 50, 60, 70, 80];
    let mut history = Vec::new();
    for _ in 0..4 {
        history.extend_from_slice(&pattern);
    }

    let ngram = {
        let mut p = NgramProposer::new();
        observe_all(&mut p, &history);
        p
    };
    let core = FixedProposer {
        tokens: vec![10, 20],
    };
    let mut composer = CascadeComposer::new(vec![Box::new(core), Box::new(ngram)]);

    let (draft, segments) = composer.compose(&history, 8);
    assert_eq!(draft, vec![10, 20, 30, 40, 50, 60, 70, 80]);
    assert_eq!(
        segments,
        vec![
            DraftSegment {
                proposer_idx: 0,
                len: 2
            },
            DraftSegment {
                proposer_idx: 1,
                len: 6
            },
        ]
    );
}

#[test]
fn cascade_respects_total_budget() {
    let core = FixedProposer {
        tokens: vec![1, 2, 3, 4, 5, 6, 7, 8],
    };
    let tail = FixedProposer {
        tokens: vec![9, 9, 9],
    };
    let mut composer = CascadeComposer::new(vec![Box::new(core), Box::new(tail)]);

    let (draft, segments) = composer.compose(&[0], 5);
    assert_eq!(draft, vec![1, 2, 3, 4, 5]);
    assert_eq!(segments.len(), 1, "budget exhausted before second proposer");

    let (draft, segments) = composer.compose(&[0], 10);
    assert_eq!(draft, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 9]);
    assert_eq!(segments[1].len, 2, "tail truncated to remaining budget");
}

#[test]
fn commit_feedback_attributes_acceptance_in_draft_order() {
    let core = FixedProposer {
        tokens: vec![1, 2, 3, 4],
    };
    let tail = FixedProposer {
        tokens: vec![5, 6, 7, 8],
    };
    let mut composer = CascadeComposer::new(vec![Box::new(core), Box::new(tail)]);

    let (draft, segments) = composer.compose(&[0], 8);
    assert_eq!(draft.len(), 8);
    // 6 accepted: core gets 4/4, tail gets 2/4.
    composer.commit_feedback(&segments, 6);

    let stats = composer.stats();
    assert_eq!((stats[0].proposed, stats[0].accepted), (4, 4));
    assert_eq!((stats[1].proposed, stats[1].accepted), (4, 2));
    assert!((stats[0].acceptance_rate - 1.0).abs() < 1e-9);
    assert!((stats[1].acceptance_rate - 0.5).abs() < 1e-9);
}

#[test]
fn adaptive_disable_kicks_in_and_recovers() {
    let bad = FixedProposer {
        tokens: vec![1, 2, 3, 4],
    };
    let mut composer = CascadeComposer::new(vec![Box::new(bad)]);

    // 32 fully-rejected drafts fill the window and trip the disable rule.
    for _ in 0..super::stats::WINDOW_CALLS {
        let (draft, segments) = composer.compose(&[0], 8);
        assert!(!draft.is_empty());
        composer.commit_feedback(&segments, 0);
    }
    assert!(composer.stats()[0].sleeping);
    let (draft, segments) = composer.compose(&[0], 8);
    assert!(
        draft.is_empty() && segments.is_empty(),
        "sleeping proposer skipped"
    );

    // Sleep is measured in committed tokens; after SLEEP_TOKENS it retries.
    for t in 0..super::stats::SLEEP_TOKENS {
        composer.observe(t as u32);
    }
    assert!(!composer.stats()[0].sleeping);
    let (draft, _) = composer.compose(&[0], 8);
    assert_eq!(draft, vec![1, 2, 3, 4], "proposer active again after sleep");
}

#[test]
fn high_acceptance_never_disables() {
    let good = FixedProposer {
        tokens: vec![1, 2, 3, 4],
    };
    let mut composer = CascadeComposer::new(vec![Box::new(good)]);
    for _ in 0..3 * super::stats::WINDOW_CALLS {
        let (draft, segments) = composer.compose(&[0], 8);
        composer.commit_feedback(&segments, draft.len());
    }
    let stats = &composer.stats()[0];
    assert!(!stats.sleeping);
    assert!((stats.acceptance_rate - 1.0).abs() < 1e-9);
}

#[test]
fn stats_math_totals_and_rate() {
    let mut s = ProposerStats::new();
    s.record(10, 5);
    s.record(4, 4);
    s.record(0, 0); // empty proposals carry no signal
    assert_eq!(s.proposed(), 14);
    assert_eq!(s.accepted(), 9);
    assert!((s.acceptance_rate() - 9.0 / 14.0).abs() < 1e-9);
    assert!(!s.is_sleeping());

    let fresh = ProposerStats::new();
    assert_eq!(fresh.acceptance_rate(), 0.0);
}

#[test]
fn speculative_state_draft_commit_roundtrip() {
    let pattern: Vec<u32> = vec![3, 1, 4, 1, 5, 9, 2, 6];
    let mut prompt = Vec::new();
    for _ in 0..6 {
        prompt.extend_from_slice(&pattern);
    }

    let composer = CascadeComposer::new(vec![Box::new(NgramProposer::new())]);
    let mut state = SpeculativeState::new(composer);
    state.observe_all(&prompt);

    let draft = state.draft(8);
    assert_eq!(draft, pattern);

    // The model "actually" continues the pattern for 5 tokens then diverges.
    let sampled: Vec<u32> = vec![3, 1, 4, 1, 5, 7, 7, 7];
    let accepted = verify_greedy(&draft, &sampled);
    assert_eq!(accepted, 5);
    state.commit(&draft, accepted);
    // Bonus token: the model's own sample at the first rejected position.
    state.observe(sampled[accepted]);

    assert_eq!(state.history().len(), prompt.len() + accepted + 1);
    let stats = state.stats();
    assert_eq!((stats[0].proposed, stats[0].accepted), (8, 5));
}

#[test]
fn property_ngram_proposals_always_match_a_recurrence() {
    for seed in 0..5u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e3779b97f4a7c15).wrapping_add(1));
        let mut p = NgramProposer::new();
        for step in 0..2000u32 {
            p.observe(rng.below(8) as u32);
            if step % 16 != 0 {
                continue;
            }
            let budget = 1 + rng.below(8) as usize;
            let history: Vec<u32> = p.observed().to_vec();
            let draft = p.propose(&SeqContext::new(&history), budget);
            assert!(draft.len() <= budget);
            if draft.is_empty() {
                continue;
            }
            // Invariant: the draft is copied verbatim from an earlier point
            // in observed history whose preceding token matches the context
            // tail (the matched gram always ends with the last ctx token).
            let last = *history.last().expect("history is non-empty");
            let found = (1..history.len()).any(|pos| {
                history[pos - 1] == last
                    && pos + draft.len() <= history.len()
                    && history[pos..pos + draft.len()] == draft[..]
            });
            assert!(found, "seed {seed} step {step}: draft is not a recurrence");
        }
    }
}

#[test]
fn property_state_commit_observe_keeps_index_consistent() {
    for seed in 0..5u64 {
        let mut rng = Rng(seed.wrapping_add(42));
        let composer = CascadeComposer::new(vec![Box::new(NgramProposer::new())]);
        let mut state = SpeculativeState::new(composer);

        let mut expected_len = 0usize;
        for _ in 0..64 {
            state.observe(rng.below(6) as u32);
            expected_len += 1;
        }

        let mut total_accepted = 0u64;
        for _ in 0..200 {
            let budget = 1 + rng.below(6) as usize;
            let draft = state.draft(budget);
            assert!(draft.len() <= budget);
            let n_accepted = if draft.is_empty() {
                0
            } else {
                rng.below(draft.len() as u64 + 1) as usize
            };
            state.commit(&draft, n_accepted);
            expected_len += n_accepted;
            total_accepted += n_accepted as u64;
            // Engine always samples one real token per verify step.
            state.observe(rng.below(6) as u32);
            expected_len += 1;
        }

        assert_eq!(state.history().len(), expected_len);
        let stats = state.stats();
        assert_eq!(
            stats.iter().map(|s| s.accepted).sum::<u64>(),
            total_accepted,
            "seed {seed}: acceptance attribution must be lossless"
        );
    }
}
