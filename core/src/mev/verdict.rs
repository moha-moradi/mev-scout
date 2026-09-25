//! Detector-vs-realized MEV profit verdict (MEV-VERIFICATION §A).
//!
//! Compares the *detector's* expected net profit (`expected_profit −
//! gas_cost_wei`, native wei) against the realized on-chain result for the
//! same `tx_hash` (trace-observed native balance delta). This is distinct from
//! [`crate::jobs::trace_verdict`], which checks the *classifier's* USD call
//! against the same trace evidence. A detector can be honestly estimated but
//! the classifier's price attribution wrong, and vice-versa — the two gates
//! are independent.

use serde::Serialize;

/// Outcome of comparing a detector-expected net profit against realized.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum MevVerdict {
    /// Detector and realized agree within tolerance.
    Pass,
    /// Detector over/under-estimates the realized net profit beyond tolerance.
    Fail(String),
    /// One side of the comparison is missing (no realized trace delta, no
    /// detector row) — degraded coverage, not a failure.
    Unverifiable(String),
}

impl MevVerdict {
    /// Short machine-readable tag.
    pub fn as_str(&self) -> &'static str {
        match self {
            MevVerdict::Pass => "pass",
            MevVerdict::Fail(_) => "fail",
            MevVerdict::Unverifiable(_) => "unverifiable",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            MevVerdict::Pass => None,
            MevVerdict::Fail(r) | MevVerdict::Unverifiable(r) => Some(r),
        }
    }
}

impl std::fmt::Display for MevVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MevVerdict::Pass => write!(f, "pass"),
            MevVerdict::Fail(r) => write!(f, "fail: {r}"),
            MevVerdict::Unverifiable(r) => write!(f, "unverifiable: {r}"),
        }
    }
}

/// Pure verdict function for the detector-vs-realized gate (offline-testable;
/// the CLI layer assembles the inputs).
///
/// Both amounts are native wei: the detector's `expected_profit` is
/// native-normalized for the ledger-eligible strategies (see
/// [`crate::paper::ledger`]) and `gas_cost_wei` is native; realized is the
/// trace-observed native balance delta. `err_pct` is caller-computed as
/// `(expected − realized)/|expected| × 100` — positive means the detector
/// over-estimates the on-chain result.
///
/// - `Pass` when `|err_pct| <= tolerance_pct`, or when the expected net is
///   ~0 (`err_pct` unknown) and `|expected − realized| <= abs_wei_tol`.
/// - `Fail(reason)` when `|err_pct| > tolerance_pct`.
/// - `Unverifiable(reason)` when either side of the comparison is missing.
pub fn mev_verdict(
    expected_net_wei: Option<i128>,
    realized_net_wei: Option<i128>,
    err_pct: Option<f64>,
    tolerance_pct: f64,
    abs_wei_tol: i128,
) -> MevVerdict {
    let Some(expected) = expected_net_wei else {
        return MevVerdict::Unverifiable(
            "no detector expected net profit for this tx — opportunity row lacks \
             expected_profit or gas_cost_wei"
                .to_string(),
        );
    };
    let Some(realized) = realized_net_wei else {
        return MevVerdict::Unverifiable(
            "no realized native delta for this tx — run `explorer show --trace` first".to_string(),
        );
    };
    match err_pct {
        Some(pct) => {
            if pct.abs() <= tolerance_pct {
                MevVerdict::Pass
            } else {
                let direction = if pct > 0.0 {
                    "over-estimate"
                } else {
                    "under-estimate"
                };
                MevVerdict::Fail(format!(
                    "detector profit_error_pct {pct:+.1}% exceeds tolerance ±{tolerance_pct:.1}% \
                     (detector {direction} vs on-chain result)"
                ))
            }
        }
        None => {
            // `err_pct` is undefined when expected ≈ 0; fall back to an absolute
            // wei band so a near-zero baseline cannot fail at an arbitrary %.
            let diff = (expected - realized).abs();
            if diff <= abs_wei_tol {
                MevVerdict::Pass
            } else {
                MevVerdict::Fail(format!(
                    "expected ≈ 0 but |expected − realized| = {diff} wei > {abs_wei_tol} wei \
                     absolute tolerance"
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirror of the trace gate's five-case matrix (`jobs::trace` tests).
    const TOL: f64 = 20.0;
    // 0.005 native — only exercised in the degenerate expected ≈ 0 branch.
    const ABS: i128 = 5_000_000_000_000_000;
    const WEI: i128 = 1_000_000_000_000_000_000;

    #[test]
    fn mev_verdict_passes_within_tolerance() {
        assert_eq!(
            mev_verdict(Some(WEI), Some(WEI - WEI / 10), Some(10.0), TOL, ABS),
            MevVerdict::Pass
        );
        // exactly at the boundary passes.
        assert_eq!(
            mev_verdict(Some(WEI), Some(WEI - WEI / 5), Some(20.0), TOL, ABS),
            MevVerdict::Pass
        );
    }

    #[test]
    fn mev_verdict_fails_over_estimate() {
        let v = mev_verdict(Some(WEI), Some(WEI / 2), Some(50.0), TOL, ABS);
        let MevVerdict::Fail(r) = v else {
            panic!("expected Fail");
        };
        assert!(r.contains("over-estimate"), "{r}");
        assert_eq!(
            mev_verdict(Some(WEI), Some(WEI / 2), Some(50.0), TOL, ABS).as_str(),
            "fail"
        );
    }

    #[test]
    fn mev_verdict_fails_under_estimate() {
        let v = mev_verdict(Some(WEI), Some(2 * WEI), Some(-100.0), TOL, ABS);
        let MevVerdict::Fail(r) = v else {
            panic!("expected Fail");
        };
        assert!(r.contains("under-estimate"), "{r}");
    }

    #[test]
    fn mev_verdict_uses_abs_band_when_expected_near_zero() {
        // expected = 0 → err_pct undefined; 0 vs 0 passes outright, small
        // residuals within the band.
        assert_eq!(
            mev_verdict(Some(0), Some(0), None, TOL, ABS),
            MevVerdict::Pass
        );
        assert_eq!(
            mev_verdict(Some(0), Some(ABS - 1), None, TOL, ABS),
            MevVerdict::Pass
        );
        // A delta far beyond the band fails.
        let MevVerdict::Fail(r) = mev_verdict(Some(0), Some(WEI), None, TOL, ABS) else {
            panic!("expected Fail");
        };
        assert!(r.contains("expected ≈ 0"), "{r}");
    }

    #[test]
    fn mev_verdict_unverifiable_without_realized() {
        assert_eq!(
            mev_verdict(Some(WEI), None, None, TOL, ABS),
            MevVerdict::Unverifiable(
                "no realized native delta for this tx — run `explorer show --trace` first"
                    .to_string()
            )
        );
    }

    #[test]
    fn mev_verdict_unverifiable_without_expected() {
        let v = mev_verdict(None, Some(WEI / 2), None, TOL, ABS);
        let MevVerdict::Unverifiable(r) = v else {
            panic!("expected Unverifiable");
        };
        assert!(r.contains("no detector expected net profit"), "{r}");
    }
}
