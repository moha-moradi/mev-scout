//! Canonical pool-fee units — kills the bps/ppm overload (W5.2).
//!
//! `PoolInfo.fee` stores a raw `u32` whose unit is implied by DEX family:
//! basis points (30) for constant-product pools, parts-per-million (3000) for
//! concentrated-liquidity and stable pools, and `0` doubles as the "unset"
//! sentinel during discovery. Before this module every reader re-derived the
//! unit from its match arm, so a fee read in the wrong arm silently
//! misquoted by a factor of 100.
//!
//! `FeeTier` captures the interpretation once, next to the definition, and
//! every quoting function takes it as a parameter — a bps value can no longer
//! be passed where ppm is expected.

use crate::dex_type::DexType;

/// Fee denominator for basis-point fees (100% = 10_000 bps).
pub const BPS_FEE_DENOM: u128 = 10_000;
/// Fee denominator for parts-per-million fees (100% = 1_000_000 ppm).
pub const PPM_FEE_DENOM: u128 = 1_000_000;

/// A pool fee with an explicit unit, derived from the DEX family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeeTier {
    /// Fee not resolved yet (discovery sentinel); treat as free until repaired.
    #[default]
    Unset,
    /// Zero-fee pool (Pendle is simulated fee-free upstream).
    Free,
    /// Basis points: 30 bps = 0.3% (V2 forks, Solidly volatile, LB).
    Bps(u32),
    /// Parts per million: 3000 ppm = 0.3% (V3/V4/Infinity CL, Curve, Balancer).
    Ppm(u32),
}

impl FeeTier {
    /// Interpret a raw `PoolInfo.fee` value for the given DEX type.
    ///
    /// This is the single canonical unit map. Adding a DEX means adding one
    /// arm here instead of a unit convention at every read site.
    pub fn from_raw(dex_type: DexType, raw_fee: u32) -> Self {
        if raw_fee == 0 {
            return match dex_type {
                // Pendle quotes are fee-free upstream.
                DexType::Pendle => FeeTier::Free,
                _ => FeeTier::Unset,
            };
        }
        match dex_type {
            DexType::UniswapV2 | DexType::Solidly | DexType::Camelot | DexType::TraderJoeLB => {
                FeeTier::Bps(raw_fee)
            }
            DexType::UniswapV3
            | DexType::UniswapV4
            | DexType::PancakeInfinity
            | DexType::Curve
            | DexType::Balancer => FeeTier::Ppm(raw_fee),
            // Pendle fee handling lives in the AMM formula, not here.
            DexType::Pendle => FeeTier::Free,
            // Metric/Fluid quoting is disabled; the fee is unused.
            DexType::Metric | DexType::Fluid => FeeTier::Unset,
        }
    }

    /// Fee as an exact fraction (numerator, denominator): `(30, 10_000)`.
    pub fn fraction(self) -> (u128, u128) {
        match self {
            FeeTier::Unset | FeeTier::Free => (0, BPS_FEE_DENOM),
            FeeTier::Bps(f) => (f as u128, BPS_FEE_DENOM),
            FeeTier::Ppm(f) => (f as u128, PPM_FEE_DENOM),
        }
    }

    /// Multiplicative fee factor as an exact rational:
    /// `(denom - fee, denom)` — the fraction of input kept after the fee.
    pub fn kept_fraction(self) -> (u128, u128) {
        let (num, den) = self.fraction();
        (den - num.min(den), den)
    }

    /// Fee factor for f64 approximations: `1.0 - fee/denom`.
    pub fn kept_factor_f64(self) -> f64 {
        let (num, den) = self.fraction();
        1.0 - num as f64 / den as f64
    }

    /// Exact integer fee charged on `amount_in` (truncated).
    pub fn fee_on_amount(self, amount_in: u128) -> u128 {
        let (num, den) = self.fraction();
        amount_in.saturating_mul(num) / den
    }

    /// Raw persisted value for the given DEX type (inverse of `from_raw`).
    pub fn to_raw(self) -> u32 {
        match self {
            FeeTier::Unset | FeeTier::Free => 0,
            FeeTier::Bps(f) => f,
            FeeTier::Ppm(f) => f,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_match_dex_family() {
        // V2-family pools store bps; remote discovery uses fee=0 as fallback.
        assert_eq!(FeeTier::from_raw(DexType::UniswapV2, 30), FeeTier::Bps(30));
        assert_eq!(FeeTier::from_raw(DexType::Solidly, 5), FeeTier::Bps(5));
        assert_eq!(
            FeeTier::from_raw(DexType::TraderJoeLB, 20),
            FeeTier::Bps(20)
        );
        // Concentrated-liquidity and stable pools store ppm.
        assert_eq!(
            FeeTier::from_raw(DexType::UniswapV3, 3000),
            FeeTier::Ppm(3000)
        );
        assert_eq!(
            FeeTier::from_raw(DexType::Balancer, 1000),
            FeeTier::Ppm(1000)
        );
        assert_eq!(FeeTier::from_raw(DexType::Curve, 40), FeeTier::Ppm(40));
    }

    #[test]
    fn zero_is_unset_except_pendle_free() {
        assert_eq!(FeeTier::from_raw(DexType::UniswapV2, 0), FeeTier::Unset);
        assert_eq!(FeeTier::from_raw(DexType::UniswapV3, 0), FeeTier::Unset);
        // Pendle's fee is baked into the AMM; raw 0 stays free.
        assert_eq!(FeeTier::from_raw(DexType::Pendle, 0), FeeTier::Free);
    }

    #[test]
    fn fractions_and_factors() {
        let v2 = FeeTier::Bps(30);
        assert_eq!(v2.fraction(), (30, 10_000));
        assert_eq!(v2.kept_fraction(), (9_970, 10_000));
        let v3 = FeeTier::Ppm(3000);
        assert_eq!(v3.fraction(), (3000, 1_000_000));
        assert_eq!(v3.kept_factor_f64(), 0.997);
        // Free/Unset never produce a negative kept factor.
        assert_eq!(FeeTier::Free.kept_fraction(), (10_000, 10_000));
        assert_eq!(FeeTier::Unset.fee_on_amount(1_000), 0);
    }

    #[test]
    fn raw_round_trip() {
        assert_eq!(FeeTier::Bps(30).to_raw(), 30);
        assert_eq!(FeeTier::Ppm(3000).to_raw(), 3000);
        assert_eq!(FeeTier::Unset.to_raw(), 0);
    }
}
