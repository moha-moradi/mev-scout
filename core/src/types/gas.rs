//! Observed-gas calibration (#7): rolling averages of actual transaction
//! `gasUsed` bucketed by dominant DEX type and pool count, used to replace
//! static per-pool gas constants in opportunity gas estimation.

use std::collections::HashMap;

use crate::dex_type::DexType;

/// Minimum samples before a bucket is considered calibrated.
pub const MIN_CALIBRATION_SAMPLES: u32 = 30;
/// Hop-count buckets are capped at this value (higher shapes fold in).
const MAX_HOP_INDEX: usize = 8;
/// Slack for all `DexType` discriminants (currently 0..=9).
const DEX_SLOTS: usize = 16;
/// Hop slots: pool counts 0..=MAX_HOP_INDEX.
const HOP_SLOTS: usize = MAX_HOP_INDEX + 1;

fn bucket_index(dex: DexType, pools_touched: usize) -> Option<(usize, usize)> {
    let d = dex as i64;
    if d < 0 {
        return None;
    }
    let d = d as usize;
    (d < DEX_SLOTS).then_some((d, pools_touched.min(MAX_HOP_INDEX)))
}

/// Mutable collector for observed transaction gas usage.
///
/// The backtest runner records `(dominant dex type, pools touched, tx gasUsed)`
/// for every successful DEX-touching transaction during replay; a snapshot is
/// handed to detectors each block via [`crate::types::GasConfig::calibration`].
#[derive(Debug, Clone, Default)]
pub struct GasCalibration {
    buckets: HashMap<(DexType, u8), Bucket>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Bucket {
    samples: u32,
    total_gas: u64,
}

impl GasCalibration {
    /// Record one observed transaction.
    ///
    /// `pools_touched` is the number of distinct tracked pools whose events the
    /// transaction emitted - the natural proxy for hop count / execution shape.
    pub fn record(&mut self, dex: DexType, pools_touched: usize, gas_used: u64) {
        if gas_used == 0 {
            return;
        }
        let key = (dex, (pools_touched.min(usize::from(u8::MAX))) as u8);
        let b = self.buckets.entry(key).or_default();
        b.samples = b.samples.saturating_add(1);
        b.total_gas = b.total_gas.saturating_add(gas_used);
    }

    /// Freeze an immutable view for detector use.
    pub fn snapshot(&self) -> GasCalibrationSnapshot {
        let mut snap = GasCalibrationSnapshot::default();
        for (&(dex, hops), &b) in &self.buckets {
            if b.samples == 0 {
                continue;
            }
            let Some((d, h)) = bucket_index(dex, hops as usize) else {
                continue;
            };
            snap.samples[d][h] += b.samples;
            snap.total_gas[d][h] += b.total_gas;
        }
        snap
    }

    /// Number of populated buckets (used by tests/telemetry).
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    /// Whether no observation has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }
}

/// Immutable per-block calibration view embedded in [`crate::types::GasConfig`].
///
/// Fixed-capacity storage keeps the type `Copy`, so `GasConfig` stays `Copy`
/// and existing detector call sites pass it by value unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub struct GasCalibrationSnapshot {
    /// Indexed `[dex_slot][hop_slot]`.
    samples: [[u32; HOP_SLOTS]; DEX_SLOTS],
    /// Indexed `[dex_slot][hop_slot]`.
    total_gas: [[u64; HOP_SLOTS]; DEX_SLOTS],
}

impl GasCalibrationSnapshot {
    fn observed_mean(&self, dex: DexType, hops: usize) -> Option<(u32, u64)> {
        let (d, h) = bucket_index(dex, hops)?;
        let n = self.samples[d][h];
        (n > 0).then(|| (n, self.total_gas[d][h] / n as u64))
    }

    /// Blend an analytically-derived gas limit with the observed mean for the
    /// matching `(dex, hop-count)` bucket once at least
    /// [`MIN_CALIBRATION_SAMPLES`] transactions have been seen.
    ///
    /// The observed mean is clamped to ±100% of the analytic estimate so outlier
    /// transactions (multi-swap routers, partial fills) cannot distort
    /// estimates; when uncalibrated the analytic value passes through unchanged.
    pub fn blended_gas_limit(&self, dex: DexType, hops: usize, analytic: u64) -> u64 {
        if analytic == 0 {
            return 0;
        }
        let Some((samples, mean)) = self.observed_mean(dex, hops) else {
            return analytic;
        };
        if samples < MIN_CALIBRATION_SAMPLES || mean == 0 {
            return analytic;
        }
        let lo = (analytic / 2).max(1);
        let hi = analytic.saturating_mul(2);
        mean.clamp(lo, hi)
    }

    /// Observed sample count and mean for a bucket, if any.
    pub fn observed(&self, dex: DexType, hops: usize) -> Option<(u32, u64)> {
        self.observed_mean(dex, hops)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncalibrated_passes_through() {
        let snap = GasCalibrationSnapshot::default();
        assert_eq!(
            snap.blended_gas_limit(DexType::UniswapV2, 2, 300_000),
            300_000
        );
        assert!(snap.observed(DexType::UniswapV2, 2).is_none());
    }

    #[test]
    fn below_min_samples_passes_through() {
        let mut calib = GasCalibration::default();
        for _ in 0..(MIN_CALIBRATION_SAMPLES - 1) {
            calib.record(DexType::UniswapV2, 2, 500_000);
        }
        let snap = calib.snapshot();
        assert_eq!(
            snap.blended_gas_limit(DexType::UniswapV2, 2, 300_000),
            300_000
        );
    }

    #[test]
    fn calibrated_mean_replaces_when_in_band() {
        let mut calib = GasCalibration::default();
        for _ in 0..MIN_CALIBRATION_SAMPLES {
            calib.record(DexType::UniswapV3, 2, 400_000);
        }
        let snap = calib.snapshot();
        assert_eq!(
            snap.blended_gas_limit(DexType::UniswapV3, 2, 350_000),
            400_000,
            "observed mean within ±100% band replaces analytic estimate"
        );
        assert_eq!(
            snap.observed(DexType::UniswapV3, 2),
            Some((MIN_CALIBRATION_SAMPLES, 400_000))
        );
    }

    #[test]
    fn outliers_clamped_to_band() {
        let mut calib = GasCalibration::default();
        // One extreme outlier among many pulls the mean above 2x analytic.
        for _ in 0..(MIN_CALIBRATION_SAMPLES - 1) {
            calib.record(DexType::Balancer, 3, 200_000);
        }
        calib.record(DexType::Balancer, 3, 5_000_000);
        let snap = calib.snapshot();
        // mean = 360k > hi = 2 x 150k -> clamped to 300k.
        assert_eq!(
            snap.blended_gas_limit(DexType::Balancer, 3, 150_000),
            300_000
        );
    }

    #[test]
    fn zero_gas_records_are_ignored() {
        let mut calib = GasCalibration::default();
        calib.record(DexType::Curve, 1, 0);
        assert!(calib.is_empty());
    }

    #[test]
    fn snapshot_is_copy_and_bounds_safe() {
        let mut calib = GasCalibration::default();
        calib.record(DexType::Pendle, 50, 123_456); // folds into hop slot 8
        let snap = calib.snapshot();
        let copied = snap; // Copy semantics
        assert_eq!(copied.observed(DexType::Pendle, 50), Some((1, 123_456)));
        assert_eq!(copied.observed(DexType::Pendle, 999), Some((1, 123_456)));
    }
}
