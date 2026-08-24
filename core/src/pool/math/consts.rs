pub const BPS_DENOMINATOR: u128 = 10_000;
pub const PPM_DENOMINATOR: u128 = 1_000_000;
pub const PERMILLE_DENOMINATOR: u128 = 1_000;
pub const DEFAULT_V3_FEE: u32 = 3000;
pub const DEFAULT_V3_TICK_SPACING: i32 = 60;
pub const SQRT_RATIO_CACHE_CAPACITY: usize = 4096;
pub const LIQUIDITY_FRACTION_DENOM: u128 = 100;
pub const STABLE_SWAP_A_COEFF_SOLIDLY: u32 = 200;
pub const STABLE_SWAP_A_COEFF_CAMELOT: u32 = 100;
pub const WEI_PER_ETHER: u128 = 1_000_000_000_000_000_000;
pub const GWEI_TO_WEI: u128 = 1_000_000_000;
pub const BASE_TX_GAS: u64 = 40_000;
pub const DEFAULT_POOL_GAS: u64 = 80_000;
pub const V3_POOL_GAS: u64 = 120_000;
pub const STABLE_POOL_GAS: u64 = 100_000;
pub const JIT_OVERHEAD: u64 = 150_000;
pub const LIQUIDATION_GAS_LIMIT: u64 = 180_000;

// Iteration bounds for numerical solvers
pub const TERNARY_SEARCH_ITERATIONS: i32 = 80;
pub const GOLDEN_SECTION_REFINE_ITERATIONS: usize = 40;
pub const NEWTON_INVARIANT_ITERATIONS: i32 = 128;
pub const NEWTON_OUTPUT_ITERATIONS: i32 = 64;

// Pool math thresholds
pub const MAX_V2_RESERVE_RATIO: u128 = 100;
pub const Q128_SHIFT: u32 = 128; // Q128.128 fixed-point shift (V3 fee growth)
pub const MIN_DAMPING_PERMILLE: u128 = 200;
pub const MAX_EXTRACTION_NUMERATOR: u128 = 999;
pub const BALANCER_FEE_ETHER_DIVISOR: u128 = 1_000_000_000_000;

// Discovery / backtest constants
pub const LIQUIDITY_CHANGE_THRESHOLD_DIVISOR: u128 = 1000;
pub const PERCENT_DENOMINATOR: u128 = 100;
