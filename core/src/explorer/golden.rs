//! Phase 0.5 labeled golden set for Backrun / Frontrun ship-gate scoring.
//!
//! Opportunity-side strategies do not map to these kinds, so
//! `explorer validate` cannot measure their precision via scanner matching.
//! This module holds a small synthetic labeled set (positives + explicit
//! negatives) and scores `classify_block` against those labels.
//!
//! Chain-curated labels (hand-reviewed blocks) can be layered on later; until
//! then this set is the CI ship gate for causal kinds.

use alloy::primitives::{address, Address, B256, U256};
use serde::Serialize;

use crate::explorer::classify::{classify_block, BlockInput, TxInput};
use crate::explorer::profit::ProfitTokenPolicy;
use crate::explorer::types::{Amm, MevKind, SwapFact, TransferFact};

const USDC: Address = address!("4000000000000000000000000000000000000005");
const TOKA: Address = address!("5000000000000000000000000000000000000006");
const POOL_A: Address = address!("3000000000000000000000000000000000000003");
const POOL_B: Address = address!("3000000000000000000000000000000000000004");
const ATK: Address = address!("1000000000000000000000000000000000000001");
const VICTIM: Address = address!("2000000000000000000000000000000000000002");
const MARKET: Address = address!("8000000000000000000000000000000000000008");
const WNATIVE: Address = address!("6000000000000000000000000000000000000007");

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

fn swap(pool: Address, tin: Address, tout: Address, ain: u64, aout: u64) -> SwapFact {
    SwapFact {
        tx_index: 0,
        log_index: 0,
        pool,
        amm: Amm::V2,
        token_in: tin,
        token_out: tout,
        amount_in: U256::from(ain),
        amount_out: U256::from(aout),
        tick: None,
        owner: None,
    }
}

fn transfer(li: u64, token: Address, from: Address, to: Address, amt: u64) -> TransferFact {
    TransferFact {
        tx_index: 0,
        log_index: li,
        token,
        from,
        to,
        amount: U256::from(amt),
    }
}

fn tx(
    idx: u64,
    from: Address,
    swaps: Vec<SwapFact>,
    transfers: Vec<TransferFact>,
) -> TxInput {
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
        jit: vec![],
    }
}

fn block(txs: Vec<TxInput>) -> BlockInput {
    BlockInput {
        block: 42_000,
        ts: 1_700_000_000,
        wrapped_native: WNATIVE,
        profit_policy: ProfitTokenPolicy {
            priority: vec![USDC],
            wrapped_native: WNATIVE,
            weth: WNATIVE,
        },
        arb_likely_parity: true,
        open_positions: vec![],
        txs,
    }
}

/// Embedded Phase 0.5 labeled set (synthetic positives + §13/§24 negatives).
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
                        transfer(0, TOKA, VICTIM, POOL_A, 100),
                        transfer(1, USDC, POOL_A, VICTIM, 110),
                    ],
                ),
                tx(
                    1,
                    MARKET,
                    vec![swap(POOL_A, USDC, TOKA, 1000, 100)],
                    vec![
                        transfer(0, USDC, MARKET, POOL_A, 1000),
                        transfer(1, TOKA, POOL_A, MARKET, 100),
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
                        transfer(0, TOKA, ATK, POOL_A, 100),
                        transfer(1, USDC, POOL_A, ATK, 120),
                        transfer(2, USDC, ATK, POOL_B, 120),
                        transfer(3, TOKA, POOL_B, ATK, 130),
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
                        transfer(0, USDC, MARKET, POOL_A, 1000),
                        transfer(1, TOKA, POOL_A, MARKET, 100),
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
                        transfer(0, TOKA, ATK, POOL_A, 100),
                        transfer(1, USDC, POOL_A, ATK, 120),
                        transfer(2, USDC, ATK, POOL_B, 120),
                        transfer(3, TOKA, POOL_B, ATK, 130),
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
                        transfer(0, TOKA, VICTIM, POOL_A, 100),
                        transfer(1, USDC, POOL_A, VICTIM, 110),
                    ],
                ),
                tx(
                    1,
                    ATK,
                    vec![swap(POOL_A, USDC, TOKA, 500, 100)],
                    vec![
                        transfer(0, USDC, ATK, POOL_A, 500),
                        transfer(1, TOKA, POOL_A, ATK, 100),
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
                        transfer(0, TOKA, ATK, POOL_A, 100),
                        transfer(1, USDC, POOL_A, ATK, 120),
                        transfer(2, USDC, ATK, POOL_B, 120),
                        transfer(3, TOKA, POOL_B, ATK, 130),
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
                        transfer(0, USDC, ATK, POOL_A, 100),
                        transfer(1, TOKA, POOL_A, ATK, 200),
                    ],
                ),
                tx(
                    1,
                    VICTIM,
                    vec![swap(POOL_A, USDC, TOKA, 100, 150)],
                    vec![
                        transfer(0, USDC, VICTIM, POOL_A, 100),
                        transfer(1, TOKA, POOL_A, VICTIM, 150),
                    ],
                ),
                tx(
                    2,
                    ATK,
                    vec![swap(POOL_B, TOKA, USDC, 200, 250)],
                    vec![
                        transfer(0, TOKA, ATK, POOL_B, 200),
                        transfer(1, USDC, POOL_B, ATK, 250),
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
                        transfer(0, USDC, ATK, POOL_A, 100),
                        transfer(1, TOKA, POOL_A, ATK, 200),
                    ],
                ),
                tx(
                    1,
                    VICTIM,
                    vec![swap(POOL_A, USDC, TOKA, 100, 200)],
                    vec![
                        transfer(0, USDC, VICTIM, POOL_A, 100),
                        transfer(1, TOKA, POOL_A, VICTIM, 200),
                    ],
                ),
                tx(
                    2,
                    ATK,
                    vec![swap(POOL_B, TOKA, USDC, 200, 250)],
                    vec![
                        transfer(0, TOKA, ATK, POOL_B, 200),
                        transfer(1, USDC, POOL_B, ATK, 250),
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
                        transfer(0, USDC, ATK, POOL_A, 100),
                        transfer(1, TOKA, POOL_A, ATK, 200),
                    ],
                ),
                tx(
                    1,
                    VICTIM,
                    vec![swap(POOL_A, USDC, TOKA, 200, 350)],
                    vec![
                        transfer(0, USDC, VICTIM, POOL_A, 200),
                        transfer(1, TOKA, POOL_A, VICTIM, 350),
                    ],
                ),
                tx(
                    2,
                    ATK,
                    vec![swap(POOL_A, TOKA, USDC, 350, 195)],
                    vec![
                        transfer(0, TOKA, ATK, POOL_A, 350),
                        transfer(1, USDC, POOL_A, ATK, 195),
                    ],
                ),
            ]),
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

    for case in cases {
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
        assert_eq!(score.cases, 6);
        assert_eq!(score.true_positives, 2);
        assert_eq!(score.true_negatives, 4);
    }
}
