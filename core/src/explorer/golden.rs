//! Labeled golden set for Backrun / Frontrun ship-gate scoring.
//!
//! Opportunity-side strategies do not map to these kinds. This module holds a
//! small synthetic labeled set (positives + explicit negatives) and scores
//! `classify_block` against those labels (`cargo test -p mev-scout-core golden`).
//!
//! Chain-curated labels (hand-reviewed blocks) can be layered on later; until
//! then this set is the CI ship gate for causal kinds.
#![cfg_attr(feature = "test-util", allow(dead_code))]

use alloy::primitives::{address, Address, B256, U256};
use serde::Serialize;

use crate::explorer::classify::{classify_block, BlockInput, TxInput};
use crate::explorer::test_fixtures::{
    self as fixtures, swap, ATK, MARKET, POOL_A, POOL_B, TOKA, USDC, VICTIM,
};
use crate::explorer::types::{MevKind, SwapFact, TransferFact};

/// The single block number the labeled set is scored at. Fixtures share every
/// other address with `test_fixtures` so a schema/field change lands once.
const BLOCK_NUMBER: u64 = 42_000;
/// No-swap placeholder tx used to widen causal-window boundaries in the
/// out-of-window fixture cases.
const FILLER: Address = address!("9000000000000000000000000000000000000009");

/// Expected causal claim for one labeled case.
#[derive(Debug, Clone, Copy)]
pub struct LabeledExpectation {
    pub id: &'static str,
    /// `Some(Backrun|Frontrun)` for positives; `None` for negatives (no causal
    /// claim of either kind on the labeled tx).
    pub expect_kind: Option<MevKind>,
    /// When `expect_kind` is set, the tx_index that must carry the claim.
    pub expect_tx_index: Option<u64>,
}

/// One labeled block + expectation.
pub struct LabeledCase {
    pub expectation: LabeledExpectation,
    pub input: BlockInput,
}

/// Aggregate precision / recall over the labeled causal set.
#[derive(Debug, Clone, Serialize)]
pub struct GoldenSetScore {
    pub cases: usize,
    /// Identities of the evaluated cases, in set order. Asserting this list
    /// (not `cases`) makes an added fixture a named entry and a removed one
    /// visible by name.
    pub case_ids: Vec<String>,
    pub true_positives: u64,
    pub false_positives: u64,
    pub false_negatives: u64,
    pub true_negatives: u64,
    pub precision: f64,
    pub recall: f64,
    pub failures: Vec<String>,
}

impl GoldenSetScore {
    /// Ship-gate helper: no failures and defined precision/recall ≥ thresholds.
    pub fn passes(&self, min_precision: f64, min_recall: f64) -> bool {
        self.failures.is_empty()
            && self.precision + f64::EPSILON >= min_precision
            && self.recall + f64::EPSILON >= min_recall
    }

    pub fn render(&self) -> String {
        let mut out = format!(
            "Causal labeled golden set ({} cases):\n\
             \tTP={} FP={} FN={} TN={}\n\
             \tprecision={:.3}  recall={:.3}\n",
            self.cases,
            self.true_positives,
            self.false_positives,
            self.false_negatives,
            self.true_negatives,
            self.precision,
            self.recall,
        );
        if !self.failures.is_empty() {
            out.push_str("Failures:\n");
            for f in &self.failures {
                out.push_str(&format!("\t- {f}\n"));
            }
        }
        out
    }
}

fn transfer(token: Address, from: Address, to: Address, amt: u64) -> TransferFact {
    // Position-free: the log index is stamped by [`tx`].
    fixtures::transfer(0, token, from, to, amt)
}

fn tx(
    idx: u64,
    from: Address,
    mut swaps: Vec<SwapFact>,
    mut transfers: Vec<TransferFact>,
) -> TxInput {
    // The `swap`/`transfer` helpers cannot know their position, so stamp the
    // real tx index and per-tx log ordering here. This mirrors the
    // normalization `from_logs` performs on decoded facts
    // (explorer::classify); without it every fact in the labeled set claims
    // tx 0 / log 0 and the set would silently score wrong input.
    for (log_idx, s) in swaps.iter_mut().enumerate() {
        s.tx_index = idx;
        s.log_index = log_idx as u64;
    }
    for (log_idx, t) in transfers.iter_mut().enumerate() {
        t.tx_index = idx;
        t.log_index = log_idx as u64;
    }
    TxInput {
        tx_index: idx,
        tx_hash: B256::repeat_byte(idx as u8),
        from,
        to: None,
        success: true,
        gas_used: 100_000,
        effective_gas_price_gwei: 30.0,
        value: U256::ZERO,
        transfers,
        swaps,
        liquidations: vec![],
        flashloans: vec![],
        buy_collaterals: vec![],
        jit: vec![],
        v2_pair_ops: vec![],
        oracle_updates: vec![],
        reserve_updates: vec![],
        rate_cache_updates: vec![],
        keepers: vec![],
        epoch_rewards: vec![],
        gmx_events: vec![],
        user_ops: vec![],
        aggregator_signals: vec![],
    }
}

fn block(txs: Vec<TxInput>) -> BlockInput {
    BlockInput {
        block: BLOCK_NUMBER,
        ..fixtures::block(txs)
    }
}

/// No-op filler: carries no swaps or transfers, so it cannot anchor or
/// interfere with any classify pass; it only advances tx indices.
fn filler(idx: u64) -> TxInput {
    tx(idx, FILLER, vec![], vec![])
}

/// Embedded labeled set (synthetic positives + negatives).
pub fn causal_labeled_set() -> Vec<LabeledCase> {
    vec![
        // ── Backrun positive ───────────────────────────────────────────
        LabeledCase {
            expectation: LabeledExpectation {
                id: "backrun_after_market_move",
                expect_kind: Some(MevKind::Backrun),
                expect_tx_index: Some(2),
            },
            input: block(vec![
                tx(
                    0,
                    VICTIM,
                    vec![swap(POOL_A, TOKA, USDC, 100, 110)],
                    vec![
                        transfer(TOKA, VICTIM, POOL_A, 100),
                        transfer(USDC, POOL_A, VICTIM, 110),
                    ],
                ),
                tx(
                    1,
                    MARKET,
                    vec![swap(POOL_A, USDC, TOKA, 1000, 100)],
                    vec![
                        transfer(USDC, MARKET, POOL_A, 1000),
                        transfer(TOKA, POOL_A, MARKET, 100),
                    ],
                ),
                tx(
                    2,
                    ATK,
                    vec![
                        swap(POOL_A, TOKA, USDC, 100, 120),
                        swap(POOL_B, USDC, TOKA, 120, 130),
                    ],
                    vec![
                        transfer(TOKA, ATK, POOL_A, 100),
                        transfer(USDC, POOL_A, ATK, 120),
                        transfer(USDC, ATK, POOL_B, 120),
                        transfer(TOKA, POOL_B, ATK, 130),
                    ],
                ),
            ]),
        },
        // ── Backrun negatives ──────────────────────────────────────────
        LabeledCase {
            expectation: LabeledExpectation {
                id: "backrun_no_pre_move_reference",
                expect_kind: None,
                expect_tx_index: None,
            },
            input: block(vec![
                tx(
                    0,
                    MARKET,
                    vec![swap(POOL_A, USDC, TOKA, 1000, 100)],
                    vec![
                        transfer(USDC, MARKET, POOL_A, 1000),
                        transfer(TOKA, POOL_A, MARKET, 100),
                    ],
                ),
                tx(
                    1,
                    ATK,
                    vec![
                        swap(POOL_A, TOKA, USDC, 100, 120),
                        swap(POOL_B, USDC, TOKA, 120, 130),
                    ],
                    vec![
                        transfer(TOKA, ATK, POOL_A, 100),
                        transfer(USDC, POOL_A, ATK, 120),
                        transfer(USDC, ATK, POOL_B, 120),
                        transfer(TOKA, POOL_B, ATK, 130),
                    ],
                ),
            ]),
        },
        LabeledCase {
            expectation: LabeledExpectation {
                id: "backrun_same_sender_as_move",
                expect_kind: None,
                expect_tx_index: None,
            },
            input: block(vec![
                tx(
                    0,
                    VICTIM,
                    vec![swap(POOL_A, TOKA, USDC, 100, 110)],
                    vec![
                        transfer(TOKA, VICTIM, POOL_A, 100),
                        transfer(USDC, POOL_A, VICTIM, 110),
                    ],
                ),
                tx(
                    1,
                    ATK,
                    vec![swap(POOL_A, USDC, TOKA, 500, 100)],
                    vec![
                        transfer(USDC, ATK, POOL_A, 500),
                        transfer(TOKA, POOL_A, ATK, 100),
                    ],
                ),
                tx(
                    2,
                    ATK,
                    vec![
                        swap(POOL_A, TOKA, USDC, 100, 120),
                        swap(POOL_B, USDC, TOKA, 120, 130),
                    ],
                    vec![
                        transfer(TOKA, ATK, POOL_A, 100),
                        transfer(USDC, POOL_A, ATK, 120),
                        transfer(USDC, ATK, POOL_B, 120),
                        transfer(TOKA, POOL_B, ATK, 130),
                    ],
                ),
            ]),
        },
        // ── Frontrun positive ──────────────────────────────────────────
        LabeledCase {
            expectation: LabeledExpectation {
                id: "frontrun_cross_pool_close",
                expect_kind: Some(MevKind::Frontrun),
                expect_tx_index: Some(0),
            },
            input: block(vec![
                tx(
                    0,
                    ATK,
                    vec![swap(POOL_A, USDC, TOKA, 100, 200)],
                    vec![
                        transfer(USDC, ATK, POOL_A, 100),
                        transfer(TOKA, POOL_A, ATK, 200),
                    ],
                ),
                tx(
                    1,
                    VICTIM,
                    vec![swap(POOL_A, USDC, TOKA, 100, 150)],
                    vec![
                        transfer(USDC, VICTIM, POOL_A, 100),
                        transfer(TOKA, POOL_A, VICTIM, 150),
                    ],
                ),
                tx(
                    2,
                    ATK,
                    vec![swap(POOL_B, TOKA, USDC, 200, 250)],
                    vec![
                        transfer(TOKA, ATK, POOL_B, 200),
                        transfer(USDC, POOL_B, ATK, 250),
                    ],
                ),
            ]),
        },
        // ── Frontrun negative ──────────────────────────────────────────
        LabeledCase {
            expectation: LabeledExpectation {
                id: "frontrun_no_measurable_degradation",
                expect_kind: None,
                expect_tx_index: None,
            },
            input: block(vec![
                tx(
                    0,
                    ATK,
                    vec![swap(POOL_A, USDC, TOKA, 100, 200)],
                    vec![
                        transfer(USDC, ATK, POOL_A, 100),
                        transfer(TOKA, POOL_A, ATK, 200),
                    ],
                ),
                tx(
                    1,
                    VICTIM,
                    vec![swap(POOL_A, USDC, TOKA, 100, 200)],
                    vec![
                        transfer(USDC, VICTIM, POOL_A, 100),
                        transfer(TOKA, POOL_A, VICTIM, 200),
                    ],
                ),
                tx(
                    2,
                    ATK,
                    vec![swap(POOL_B, TOKA, USDC, 200, 250)],
                    vec![
                        transfer(TOKA, ATK, POOL_B, 200),
                        transfer(USDC, POOL_B, ATK, 250),
                    ],
                ),
            ]),
        },
        // ── Sandwich exclusion (neither frontrun nor backrun) ──────────
        LabeledCase {
            expectation: LabeledExpectation {
                id: "sandwich_legs_not_causal",
                expect_kind: None,
                expect_tx_index: None,
            },
            input: block(vec![
                tx(
                    0,
                    ATK,
                    vec![swap(POOL_A, USDC, TOKA, 100, 200)],
                    vec![
                        transfer(USDC, ATK, POOL_A, 100),
                        transfer(TOKA, POOL_A, ATK, 200),
                    ],
                ),
                tx(
                    1,
                    VICTIM,
                    vec![swap(POOL_A, USDC, TOKA, 200, 350)],
                    vec![
                        transfer(USDC, VICTIM, POOL_A, 200),
                        transfer(TOKA, POOL_A, VICTIM, 350),
                    ],
                ),
                tx(
                    2,
                    ATK,
                    vec![swap(POOL_A, TOKA, USDC, 350, 195)],
                    vec![
                        transfer(TOKA, ATK, POOL_A, 350),
                        transfer(USDC, POOL_A, ATK, 195),
                    ],
                ),
            ]),
        },
        // ── Backrun: closer improves less than CAUSAL_PRICE_MIN_PCT ────
        // Reference 110/100 = 1.10; closer 221/200 = 1.105 → +0.4545% < 0.5%.
        // Move (4000→400) passes the same-token size gate (400 ≥ 200), so
        // price materiality alone is the deciding gate.
        LabeledCase {
            expectation: LabeledExpectation {
                id: "backrun_sub_threshold_improvement",
                expect_kind: None,
                expect_tx_index: None,
            },
            input: block(vec![
                tx(
                    0,
                    VICTIM,
                    vec![swap(POOL_A, TOKA, USDC, 100, 110)],
                    vec![
                        transfer(TOKA, VICTIM, POOL_A, 100),
                        transfer(USDC, POOL_A, VICTIM, 110),
                    ],
                ),
                tx(
                    1,
                    MARKET,
                    vec![swap(POOL_A, USDC, TOKA, 4000, 400)],
                    vec![
                        transfer(USDC, MARKET, POOL_A, 4000),
                        transfer(TOKA, POOL_A, MARKET, 400),
                    ],
                ),
                tx(
                    2,
                    ATK,
                    vec![
                        swap(POOL_A, TOKA, USDC, 200, 221),
                        swap(POOL_B, USDC, TOKA, 221, 230),
                    ],
                    vec![
                        transfer(TOKA, ATK, POOL_A, 200),
                        transfer(USDC, POOL_A, ATK, 221),
                        transfer(USDC, ATK, POOL_B, 221),
                        transfer(TOKA, POOL_B, ATK, 230),
                    ],
                ),
            ]),
        },
        // ── Backrun: move farther than CAUSAL_WINDOW_TXS before the closer
        // (distance 19-10 = 9 > 8). Filler no-ops widen the gap; the ref is
        // also out of window, so the move loop finds no qualified `a`.
        LabeledCase {
            expectation: LabeledExpectation {
                id: "backrun_move_outside_window",
                expect_kind: None,
                expect_tx_index: None,
            },
            input: block({
                let mut txs = vec![tx(
                    0,
                    VICTIM,
                    vec![swap(POOL_A, TOKA, USDC, 100, 110)],
                    vec![
                        transfer(TOKA, VICTIM, POOL_A, 100),
                        transfer(USDC, POOL_A, VICTIM, 110),
                    ],
                )];
                for i in 1..=9 {
                    txs.push(filler(i));
                }
                txs.push(tx(
                    10,
                    MARKET,
                    vec![swap(POOL_A, USDC, TOKA, 1000, 100)],
                    vec![
                        transfer(USDC, MARKET, POOL_A, 1000),
                        transfer(TOKA, POOL_A, MARKET, 100),
                    ],
                ));
                for i in 11..=18 {
                    txs.push(filler(i));
                }
                txs.push(tx(
                    19,
                    ATK,
                    vec![
                        swap(POOL_A, TOKA, USDC, 100, 120),
                        swap(POOL_B, USDC, TOKA, 120, 130),
                    ],
                    vec![
                        transfer(TOKA, ATK, POOL_A, 100),
                        transfer(USDC, POOL_A, ATK, 120),
                        transfer(USDC, ATK, POOL_B, 120),
                        transfer(TOKA, POOL_B, ATK, 130),
                    ],
                ));
                txs
            }),
        },
        // ── Backrun: closer spends more of the shared token than the move
        // printed (same-token size gate: move.amount_out < back.amount_in).
        // Price improvement (250/200 vs 110/100) is otherwise well above
        // threshold, so size is the deciding gate.
        LabeledCase {
            expectation: LabeledExpectation {
                id: "backrun_size_mismatch",
                expect_kind: None,
                expect_tx_index: None,
            },
            input: block(vec![
                tx(
                    0,
                    VICTIM,
                    vec![swap(POOL_A, TOKA, USDC, 100, 110)],
                    vec![
                        transfer(TOKA, VICTIM, POOL_A, 100),
                        transfer(USDC, POOL_A, VICTIM, 110),
                    ],
                ),
                tx(
                    1,
                    MARKET,
                    vec![swap(POOL_A, USDC, TOKA, 5000, 100)],
                    vec![
                        transfer(USDC, MARKET, POOL_A, 5000),
                        transfer(TOKA, POOL_A, MARKET, 100),
                    ],
                ),
                tx(
                    2,
                    ATK,
                    vec![
                        swap(POOL_A, TOKA, USDC, 200, 250),
                        swap(POOL_B, USDC, TOKA, 250, 260),
                    ],
                    vec![
                        transfer(TOKA, ATK, POOL_A, 200),
                        transfer(USDC, POOL_A, ATK, 250),
                        transfer(USDC, ATK, POOL_B, 250),
                        transfer(TOKA, POOL_B, ATK, 260),
                    ],
                ),
            ]),
        },
        // ── Frontrun: victim degrades less than CAUSAL_PRICE_MIN_PCT
        // (2.09 vs 2.10 front → 0.476% < 0.5%). Close is otherwise a
        // profitable 2-pool cycle (+50 USDC), so the degradation gate alone
        // keeps this out of the claims.
        LabeledCase {
            expectation: LabeledExpectation {
                id: "frontrun_sub_threshold_degradation",
                expect_kind: None,
                expect_tx_index: None,
            },
            input: block(vec![
                tx(
                    0,
                    ATK,
                    vec![swap(POOL_A, USDC, TOKA, 100, 210)],
                    vec![
                        transfer(USDC, ATK, POOL_A, 100),
                        transfer(TOKA, POOL_A, ATK, 210),
                    ],
                ),
                tx(
                    1,
                    VICTIM,
                    vec![swap(POOL_A, USDC, TOKA, 100, 209)],
                    vec![
                        transfer(USDC, VICTIM, POOL_A, 100),
                        transfer(TOKA, POOL_A, VICTIM, 209),
                    ],
                ),
                tx(
                    2,
                    ATK,
                    vec![swap(POOL_B, TOKA, USDC, 210, 260)],
                    vec![
                        transfer(TOKA, ATK, POOL_B, 210),
                        transfer(USDC, POOL_B, ATK, 260),
                    ],
                ),
            ]),
        },
        // ── Frontrun: victim farther than CAUSAL_WINDOW_TXS after the front
        // (distance 10-0 = 10 > 8); no same-direction victim qualifies.
        LabeledCase {
            expectation: LabeledExpectation {
                id: "frontrun_victim_outside_window",
                expect_kind: None,
                expect_tx_index: None,
            },
            input: block({
                let mut txs = vec![tx(
                    0,
                    ATK,
                    vec![swap(POOL_A, USDC, TOKA, 100, 200)],
                    vec![
                        transfer(USDC, ATK, POOL_A, 100),
                        transfer(TOKA, POOL_A, ATK, 200),
                    ],
                )];
                for i in 1..=9 {
                    txs.push(filler(i));
                }
                txs.push(tx(
                    10,
                    VICTIM,
                    vec![swap(POOL_A, USDC, TOKA, 100, 150)],
                    vec![
                        transfer(USDC, VICTIM, POOL_A, 100),
                        transfer(TOKA, POOL_A, VICTIM, 150),
                    ],
                ));
                txs.push(tx(
                    11,
                    ATK,
                    vec![swap(POOL_B, TOKA, USDC, 200, 250)],
                    vec![
                        transfer(TOKA, ATK, POOL_B, 200),
                        transfer(USDC, POOL_B, ATK, 250),
                    ],
                ));
                txs
            }),
        },
    ]
}

fn causal_claims(events: &[crate::explorer::types::MevEvent]) -> Vec<(MevKind, u64)> {
    events
        .iter()
        .filter(|e| matches!(e.kind, MevKind::Backrun | MevKind::Frontrun))
        .map(|e| (e.kind, e.tx_index))
        .collect()
}

/// Score `classify_block` against the labeled causal set.
pub fn score_causal_labeled_set(cases: &[LabeledCase]) -> GoldenSetScore {
    let mut tp = 0u64;
    let mut fp = 0u64;
    let mut fn_ = 0u64;
    let mut tn = 0u64;
    let mut failures = Vec::new();
    let mut case_ids = Vec::with_capacity(cases.len());

    for case in cases {
        case_ids.push(case.expectation.id.to_string());
        let events = classify_block(&case.input);
        let claims = causal_claims(&events);
        let exp = &case.expectation;

        match exp.expect_kind {
            Some(want) => {
                let hit = claims.iter().any(|(k, idx)| {
                    *k == want
                        && match exp.expect_tx_index {
                            Some(i) => *idx == i,
                            None => true,
                        }
                });
                let extras: Vec<_> = claims
                    .iter()
                    .filter(|(k, idx)| {
                        *k != want
                            || match exp.expect_tx_index {
                                Some(i) => *idx != i,
                                None => false,
                            }
                    })
                    .collect();
                if hit && extras.is_empty() {
                    tp += 1;
                } else if hit {
                    // Correct kind present but extra causal claims → FP pressure.
                    tp += 1;
                    fp += extras.len() as u64;
                    failures.push(format!(
                        "{}: expected only {:?}@{:?}, got extras {:?}",
                        exp.id, want, exp.expect_tx_index, extras
                    ));
                } else if claims.is_empty() {
                    fn_ += 1;
                    failures.push(format!(
                        "{}: expected {:?}@{:?}, got no causal claim",
                        exp.id, want, exp.expect_tx_index
                    ));
                } else {
                    fn_ += 1;
                    fp += claims.len() as u64;
                    failures.push(format!(
                        "{}: expected {:?}@{:?}, got {:?}",
                        exp.id, want, exp.expect_tx_index, claims
                    ));
                }
            }
            None => {
                if claims.is_empty() {
                    tn += 1;
                } else {
                    fp += claims.len() as u64;
                    failures.push(format!(
                        "{}: expected no Backrun/Frontrun, got {:?}",
                        exp.id, claims
                    ));
                }
            }
        }
    }

    let precision = if tp + fp == 0 {
        1.0
    } else {
        tp as f64 / (tp + fp) as f64
    };
    let recall = if tp + fn_ == 0 {
        1.0
    } else {
        tp as f64 / (tp + fn_) as f64
    };

    GoldenSetScore {
        cases: cases.len(),
        case_ids,
        true_positives: tp,
        false_positives: fp,
        false_negatives: fn_,
        true_negatives: tn,
        precision,
        recall,
        failures,
    }
}

/// Convenience: score the embedded set.
pub fn score_embedded_causal_set() -> GoldenSetScore {
    score_causal_labeled_set(&causal_labeled_set())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_causal_set_passes_ship_gate() {
        let score = score_embedded_causal_set();
        assert!(
            score.passes(1.0, 1.0),
            "causal golden set must be perfect on the synthetic labels:\n{}",
            score.render()
        );
        // 2 positives + 9 negatives. Name the cases: adding a fixture extends
        // this list, dropping one fails by name instead of moving a magic count.
        assert_eq!(
            score.case_ids,
            vec![
                "backrun_after_market_move",
                "backrun_no_pre_move_reference",
                "backrun_same_sender_as_move",
                "frontrun_cross_pool_close",
                "frontrun_no_measurable_degradation",
                "sandwich_legs_not_causal",
                "backrun_sub_threshold_improvement",
                "backrun_move_outside_window",
                "backrun_size_mismatch",
                "frontrun_sub_threshold_degradation",
                "frontrun_victim_outside_window",
            ]
        );
        assert_eq!(score.true_positives, 2);
        assert_eq!(score.true_negatives, 9);
    }
}
