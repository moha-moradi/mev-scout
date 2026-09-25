//! Paper ↔ executed-profit reconciliation (MEV-VERIFICATION §B.2).
//!
//! `paper`'s claim is a single subtraction: `net_wei = expected_profit −
//! gas_cost_wei` (pure over analytic quotes, never executed — the modeled gas
//! from `types::gas`, not revm gas). This module compares each fill's claimed
//! net against its executed counterpart produced by the what-if executor
//! ([`crate::replay::whatif`]): the on-chain tx (or hypothetical bundle)
//! re-executed through revm, measured as the native balance delta (gas
//! included, coinbase excluded).
//!
//! Only one of the two sides is required to be present for a *decision*: a
//! fill with no executed counterpart is `Unverifiable` (degraded coverage, not
//! a failure) — e.g. a mempool-only op that never landed on-chain or a tx
//! index outside the replayed window. An executed revert is decisive `Fail`
//! evidence, not `Unverifiable`: paper booked a profit that execution would
//! not have produced.

use serde::Serialize;

use crate::paper::types::{LedgerResult, PaperFill};
use crate::replay::whatif::ExecutedNetMap;

/// Outcome of reconciling one paper fill against its executed result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ReconVerdict {
    /// Paper and executed agree within tolerance.
    Pass,
    /// Paper over/under-claims the executed result beyond tolerance, or the
    /// executed tx reverted/halted (paper booked a profit execution would not
    /// have produced).
    Fail(String),
    /// No executed counterpart exists for this fill — degraded coverage.
    Unverifiable(String),
}

impl ReconVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReconVerdict::Pass => "pass",
            ReconVerdict::Fail(_) => "fail",
            ReconVerdict::Unverifiable(_) => "unverifiable",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            ReconVerdict::Pass => None,
            ReconVerdict::Fail(r) | ReconVerdict::Unverifiable(r) => Some(r),
        }
    }
}

impl std::fmt::Display for ReconVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReconVerdict::Pass => write!(f, "pass"),
            ReconVerdict::Fail(r) => write!(f, "fail: {r}"),
            ReconVerdict::Unverifiable(r) => write!(f, "unverifiable: {r}"),
        }
    }
}

/// One reconciled paper fill with both sides of the comparison surfaced.
#[derive(Debug, Clone, Serialize)]
pub struct ReconFill {
    pub canonical_id: Option<String>,
    pub tx_index: Option<usize>,
    pub block_number: u64,
    pub strategy: String,
    /// Paper-side net (called `net_wei` on the fill).
    pub paper_net_wei: Option<i128>,
    /// What-if executed native net (gas-inclusive), when a counterpart exists.
    pub executed_net_wei: Option<i128>,
    pub executed_gas_used: Option<u64>,
    pub executed_status: Option<bool>,
    /// `(paper − executed) / |paper| × 100`; `None` when undefined (paper ≈ 0)
    /// or one side is missing.
    pub err_pct: Option<f64>,
    pub verdict: ReconVerdict,
}

/// Reconciliation report over a ledger's fills, matching the strict
/// Pass/Fail/Unverifiable convention of the `mev` verdict gate.
#[derive(Debug, Clone, Serialize)]
pub struct ReconReport {
    pub fills: Vec<ReconFill>,
    pub pass: usize,
    pub fail: usize,
    pub unverifiable: usize,
}

impl ReconReport {
    pub fn fill_count(&self) -> usize {
        self.fills.len()
    }

    /// `pass / (pass + fail)` over verifiable fills — the metric the corpus
    /// asserts a floor against. `None` when nothing was verifiable.
    pub fn verifiable_pass_rate(&self) -> Option<f64> {
        let verifiable = self.pass + self.fail;
        if verifiable == 0 {
            None
        } else {
            Some(self.pass as f64 / verifiable as f64)
        }
    }
}

/// Pure verdict for one fill — the offline-testable core (mirrors the five-case
/// matrix of `mev::verdict` for the paper domain).
fn fill_verdict(
    fill: &PaperFill,
    executed: Option<(i128, u64, bool)>,
    tolerance_pct: f64,
    abs_wei_tol: i128,
) -> ReconVerdict {
    let (executed_wei, gas_used, status) = match executed {
        Some(e) => e,
        None => {
            return ReconVerdict::Unverifiable(
                "no executed counterpart for this fill — mempool-only or unanchored op".to_string(),
            );
        }
    };
    if !status {
        return ReconVerdict::Fail(format!(
            "what-if execution reverted/halted (status=false, gas_used={gas_used}) — paper booked \
             a net of {} wei that execution would not have produced",
            fill.net_wei
        ));
    }
    let paper = fill.net_wei;
    let err_pct = if paper != 0 {
        Some((paper - executed_wei) as f64 / paper.abs() as f64 * 100.0)
    } else {
        None
    };
    match err_pct {
        Some(pct) if pct.abs() <= tolerance_pct => ReconVerdict::Pass,
        Some(pct) => {
            let direction = if pct > 0.0 {
                "over-estimate"
            } else {
                "under-estimate"
            };
            ReconVerdict::Fail(format!(
                "paper profit_error_pct {pct:+.1}% exceeds tolerance ±{tolerance_pct:.1}% \
                 (paper {direction} vs executed result)"
            ))
        }
        None => {
            let diff = (paper - executed_wei).abs();
            if diff <= abs_wei_tol {
                ReconVerdict::Pass
            } else {
                ReconVerdict::Fail(format!(
                    "paper ≈ 0 but |paper − executed| = {diff} wei > {abs_wei_tol} wei absolute \
                     tolerance"
                ))
            }
        }
    }
}

/// Reconcile every ledger fill against the executed map (keyed by
/// `canonical_id`). Pure — no I/O.
pub fn paper_vs_executed(
    ledger: &LedgerResult,
    executed: &ExecutedNetMap,
    tolerance_pct: f64,
    abs_wei_tol: i128,
) -> ReconReport {
    let mut report = ReconReport {
        fills: Vec::with_capacity(ledger.fills.len()),
        pass: 0,
        fail: 0,
        unverifiable: 0,
    };
    for fill in &ledger.fills {
        let executed_side = fill
            .canonical_id
            .as_deref()
            .and_then(|cid| executed.get(cid))
            .map(|e| (e.net_wei, e.gas_used, e.status));
        let verdict = fill_verdict(fill, executed_side, tolerance_pct, abs_wei_tol);
        match &verdict {
            ReconVerdict::Pass => report.pass += 1,
            ReconVerdict::Fail(_) => report.fail += 1,
            ReconVerdict::Unverifiable(_) => report.unverifiable += 1,
        }
        let (executed_wei, _, _) = executed_side.unwrap_or((0, 0, false));
        let err_pct = (fill.net_wei != 0 && executed_side.is_some())
            .then(|| (fill.net_wei - executed_wei) as f64 / fill.net_wei.abs() as f64 * 100.0);
        report.fills.push(ReconFill {
            canonical_id: fill.canonical_id.clone(),
            tx_index: fill.tx_index,
            block_number: fill.block_number,
            strategy: fill.strategy.clone(),
            paper_net_wei: Some(fill.net_wei),
            executed_net_wei: executed_side.map(|(w, _, _)| w),
            executed_gas_used: executed_side.map(|(_, g, _)| g),
            executed_status: executed_side.map(|(_, _, s)| s),
            err_pct,
            verdict,
        });
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paper::types::{LedgerResult, PaperFill};
    use crate::replay::whatif::ExecutedNet;

    fn fill(cid: &str, net: i128, block: u64, tx: usize) -> PaperFill {
        PaperFill {
            block_number: block,
            tx_index: Some(tx),
            canonical_id: Some(cid.to_string()),
            strategy: "TwoHopArb".into(),
            gross_wei: 0,
            gas_wei: 0,
            net_wei: net,
            wallet_before: 0,
            wallet_after: 0,
            pools: vec![],
            mempool_only: false,
        }
    }

    fn ledger(fills: Vec<PaperFill>) -> LedgerResult {
        let net = fills.iter().map(|f| f.net_wei).sum();
        LedgerResult {
            starting_gas_wei: 0,
            ending_gas_wei: 0,
            reserve_wei: 0,
            max_drawdown_wei: 0,
            fills,
            skips: vec![],
            gross_wei: 0,
            gas_wei: 0,
            net_profit_wei: net,
            best_net_wei: 0,
            start_block: None,
            end_block: None,
        }
    }

    const TOL: f64 = 20.0;
    const ABS: i128 = 5_000_000_000_000_000;
    const WEI: i128 = 1_000_000_000_000_000_000;

    #[test]
    fn recon_passes_within_tolerance() {
        let l = ledger(vec![fill("a", WEI, 1, 0)]);
        let mut exec = ExecutedNetMap::new();
        exec.insert(
            "a".into(),
            ExecutedNet {
                net_wei: WEI - WEI / 10,
                gas_used: 21_000,
                status: true,
            },
        );
        let r = paper_vs_executed(&l, &exec, TOL, ABS);
        assert_eq!(r.pass, 1);
        assert_eq!(r.fail, 0);
        assert!(matches!(r.fills[0].verdict, ReconVerdict::Pass));
    }

    #[test]
    fn recon_fails_over_claim() {
        let l = ledger(vec![fill("a", WEI, 1, 0)]);
        let mut exec = ExecutedNetMap::new();
        exec.insert(
            "a".into(),
            ExecutedNet {
                net_wei: WEI / 2,
                gas_used: 21_000,
                status: true,
            },
        );
        let r = paper_vs_executed(&l, &exec, TOL, ABS);
        assert_eq!(r.fail, 1);
        let ReconVerdict::Fail(msg) = &r.fills[0].verdict else {
            unreachable!()
        };
        assert!(msg.contains("over-estimate"), "{msg}");
    }

    #[test]
    fn recon_fails_under_claim() {
        let l = ledger(vec![fill("a", WEI, 1, 0)]);
        let mut exec = ExecutedNetMap::new();
        exec.insert(
            "a".into(),
            ExecutedNet {
                net_wei: 2 * WEI,
                gas_used: 21_000,
                status: true,
            },
        );
        let r = paper_vs_executed(&l, &exec, TOL, ABS);
        let ReconVerdict::Fail(msg) = &r.fills[0].verdict else {
            unreachable!()
        };
        assert!(msg.contains("under-estimate"), "{msg}");
    }

    #[test]
    fn recon_uses_abs_band_when_paper_near_zero() {
        let l = ledger(vec![fill("a", 0, 1, 0)]);
        let mut exec = ExecutedNetMap::new();
        exec.insert(
            "a".into(),
            ExecutedNet {
                net_wei: ABS - 1,
                gas_used: 21_000,
                status: true,
            },
        );
        let r = paper_vs_executed(&l, &exec, TOL, ABS);
        assert_eq!(r.pass, 1);
        let mut exec2 = ExecutedNetMap::new();
        exec2.insert(
            "a".into(),
            ExecutedNet {
                net_wei: WEI,
                gas_used: 21_000,
                status: true,
            },
        );
        let r2 = paper_vs_executed(&l, &exec2, TOL, ABS);
        assert_eq!(r2.fail, 1);
    }

    #[test]
    fn recon_unverifiable_without_executed_counterpart() {
        // Mempool-only fill with no executed row → degraded coverage, NOT a fail.
        let mut f = fill("a", WEI, 1, 1);
        f.mempool_only = true;
        let l = ledger(vec![f]);
        let r = paper_vs_executed(&l, &ExecutedNetMap::new(), TOL, ABS);
        assert_eq!(r.unverifiable, 1);
        assert_eq!(r.fail, 0);
        assert!(!r.fills[0].verdict.reason().unwrap().is_empty());
    }

    #[test]
    fn recon_fails_executed_revert() {
        let l = ledger(vec![fill("a", WEI, 1, 0)]);
        let mut exec = ExecutedNetMap::new();
        exec.insert(
            "a".into(),
            ExecutedNet {
                net_wei: 0,
                gas_used: 15_000,
                status: false,
            },
        );
        let r = paper_vs_executed(&l, &exec, TOL, ABS);
        assert_eq!(r.fail, 1);
        let ReconVerdict::Fail(msg) = &r.fills[0].verdict else {
            unreachable!()
        };
        assert!(msg.contains("reverted/halted"), "{msg}");
    }

    #[test]
    fn recon_pass_rate_is_over_verifiable_only() {
        let l = ledger(vec![
            fill("a", WEI, 1, 0),
            fill("b", WEI, 1, 1),
            fill("c", WEI, 1, 2),
        ]);
        let mut exec = ExecutedNetMap::new();
        exec.insert(
            "a".into(),
            ExecutedNet {
                net_wei: WEI,
                gas_used: 21_000,
                status: true,
            },
        );
        exec.insert(
            "b".into(),
            ExecutedNet {
                net_wei: WEI / 2,
                gas_used: 21_000,
                status: true,
            },
        );
        // "c" has no executed counterpart.
        let r = paper_vs_executed(&l, &exec, TOL, ABS);
        assert_eq!((r.pass, r.fail, r.unverifiable), (1, 1, 1));
        assert_eq!(r.verifiable_pass_rate(), Some(0.5));
    }
}
