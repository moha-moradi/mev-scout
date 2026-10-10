//! Core data contracts for the realized-MEV explorer.
//!
//! `MevEvent` is one classified operation anchored to a single transaction
//! (arb, liquidation, JIT, unknown); `MevBundle` groups multi-transaction
//! operations (sandwich front-run + victim(s) + back-run).

use alloy::primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};

/// MEV pattern kinds in scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MevKind {
    /// Single tx whose token-flow graph closes a cycle ending in the start token.
    ArbAtomic,
    /// Same sender opens + closes a position around a victim swap on one pool.
    Sandwich,
    /// Searcher swap(s) execute before a victim swap they profit from (chain
    /// reordering / mempool awareness), recorded per pool + victim anchor.
    Frontrun,
    /// Searcher swap(s) execute after a prior tx whose price move they harvest.
    Backrun,
    /// Known liquidation event on a configured lending pool.
    Liquidation,
    /// Concentrated-liquidity Mint+Burn around swaps in one block (fee capture).
    Jit,
    /// JIT combined with a same-tx/block arb.
    JitArb,
    /// UniV2-style `skim()`: pair outbound Transfers with no Swap/Mint/Burn/Sync.
    Skim,
    /// Profitable pattern not matching any rule (incl. probable CEX-DEX bots).
    Unknown,
}

impl MevKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MevKind::ArbAtomic => "arb_atomic",
            MevKind::Sandwich => "sandwich",
            MevKind::Frontrun => "frontrun",
            MevKind::Backrun => "backrun",
            MevKind::Liquidation => "liquidation",
            MevKind::Jit => "jit",
            MevKind::JitArb => "jit_arb",
            MevKind::Skim => "skim",
            MevKind::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<MevKind> {
        match s {
            "arb_atomic" => Some(MevKind::ArbAtomic),
            "sandwich" => Some(MevKind::Sandwich),
            "frontrun" => Some(MevKind::Frontrun),
            "backrun" => Some(MevKind::Backrun),
            "liquidation" => Some(MevKind::Liquidation),
            "jit" => Some(MevKind::Jit),
            "jit_arb" => Some(MevKind::JitArb),
            "skim" => Some(MevKind::Skim),
            "unknown" => Some(MevKind::Unknown),
            _ => None,
        }
    }
}

impl std::fmt::Display for MevKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MevKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| format!("unknown MevKind '{s}'"))
    }
}

/// Attribution confidence: `exact` = deterministic event/flow match;
/// `inferred` = heuristic attribution (score 0..1 carried in details).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    Exact,
    Inferred,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Exact => "exact",
            Confidence::Inferred => "inferred",
        }
    }
}

impl std::str::FromStr for Confidence {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "exact" => Ok(Self::Exact),
            "inferred" => Ok(Self::Inferred),
            other => Err(format!("unknown Confidence '{other}'")),
        }
    }
}

/// One classified realized-MEV operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevEvent {
    pub block: u64,
    pub ts: u64,
    /// Anchor transaction index. For sandwiches this is the front-run index
    /// (bundle anchor); full leg indices live in `MevBundle`/details.
    pub tx_index: u64,
    pub tx_hash: B256,
    pub kind: MevKind,
    /// Labeled EOA or the tx sender (searcher attribution target).
    pub searcher: Address,
    /// Contract involved (e.g. arb executor the EOA called), when distinct.
    pub contract: Option<Address>,
    /// Pools involved in the operation (route order for arbs).
    pub pools: Vec<Address>,
    /// Token the profit is denominated in (post netting).
    pub profit_token: Option<Address>,
    /// Net profit amount of `profit_token` (pre-gas), raw integer units.
    pub profit_amount: Option<U256>,
    /// All positive post-netting residuals (Phase 2.3), `(token, net)` in
    /// deterministic order. Persist USD-sums across these while
    /// `profit_token`/`profit_amount` remain the display-primary pair.
    pub profit_tokens: Vec<(Address, U256)>,
    /// USD value of the profit when pricing was available.
    pub profit_usd: Option<f64>,
    /// Gas cost in wei (gasUsed × effectiveGasPrice from the receipt).
    pub gas_cost_wei: U256,
    /// Flash-loan fee in wei when the tx used a flash loan (Phase 2.2).
    pub flashloan_fee_wei: Option<U256>,
    /// Token the flash-loan fee is denominated in (units of `flashloan_fee_wei`).
    pub flashloan_fee_token: Option<Address>,
    pub confidence: Confidence,
    /// Victim tx hashes (sandwiches).
    pub victim_hashes: Vec<B256>,
    /// Victim swap size in the pool's traded token (sandwiches), raw units.
    pub victim_swap_size: Option<U256>,
    /// Kind-specific structured details (liq assets, JIT tick range, notes).
    pub details: serde_json::Value,
}

/// A multi-transaction realized operation (sandwich bundle).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevBundle {
    pub kind: MevKind,
    pub attacker: Address,
    /// (tx_index, tx_hash, role) legs in block order.
    pub legs: Vec<BundleLeg>,
}

/// Role of a leg inside a sandwich / causal bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleLegRole {
    FrontRun,
    Victim,
    BackRun,
}

impl BundleLegRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FrontRun => "front_run",
            Self::Victim => "victim",
            Self::BackRun => "back_run",
        }
    }
}

impl std::fmt::Display for BundleLegRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for BundleLegRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "front_run" => Ok(Self::FrontRun),
            "victim" => Ok(Self::Victim),
            "back_run" => Ok(Self::BackRun),
            other => Err(format!("unknown bundle leg role '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleLeg {
    pub tx_index: u64,
    pub tx_hash: B256,
    pub role: BundleLegRole,
    pub pool: Address,
}

/// Typed liquidation economics used at persist time (avoids stringly JSON digs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiquidationDetails {
    pub collateral_asset: Address,
    pub debt_asset: Address,
    pub collateral_amount: U256,
    pub debt_to_cover: U256,
}

impl LiquidationDetails {
    /// Read liquidation economics from classifier `details` JSON when present.
    pub fn from_details(details: &serde_json::Value) -> Option<Self> {
        let parse_addr = |k: &str| {
            details
                .get(k)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Address>().ok())
        };
        let parse_amt = |k: &str| {
            details
                .get(k)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<U256>().ok())
        };
        Some(Self {
            collateral_asset: parse_addr("collateral_asset")?,
            debt_asset: parse_addr("debt_asset")?,
            collateral_amount: parse_amt("collateral_amount")?,
            debt_to_cover: parse_amt("debt_to_cover")?,
        })
    }
}

/// A decoded ERC-20 Transfer fact from a receipt log.
#[derive(Debug, Clone)]
pub struct TransferFact {
    pub tx_index: u64,
    /// Log position within the tx's receipt (stable for re-decode).
    pub log_index: u64,
    pub token: Address,
    pub from: Address,
    pub to: Address,
    pub amount: U256,
}

/// AMM family label carried on swap facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Amm {
    V2,
    V3,
    V4,
    /// Pancake Infinity centralized-liquidity pools (singleton CLPoolManager).
    Infinity,
    /// Metric V2 oracle-anchored tick/bin AMM pools.
    Metric,
    /// Fluid DEX unified-liquidity pools (Instadapp).
    Fluid,
    Curve,
    Balancer,
    Solidly,
    Lb,
    Pendle,
    /// Aggregator/DEX-router edge (0x, 1inch, Paraswap) with tokens carried
    /// explicitly by the router event.
    Aggregator,
}

impl Amm {
    pub fn as_str(self) -> &'static str {
        match self {
            Amm::V2 => "v2",
            Amm::V3 => "v3",
            Amm::V4 => "v4",
            Amm::Infinity => "infinity",
            Amm::Metric => "metric",
            Amm::Fluid => "fluid",
            Amm::Curve => "curve",
            Amm::Balancer => "balancer",
            Amm::Solidly => "solidly",
            Amm::Lb => "lb",
            Amm::Pendle => "pendle",
            Amm::Aggregator => "aggregator",
        }
    }
}

/// How `token_in` / `token_out` were bound to a swap's recorded amounts.
///
/// Pricing trusts `Registry` and `Transfer` only. `Proximity` (and a missing
/// key on legacy rows) is a guess and must not drive a USD rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegSource {
    /// Both sides came from the pool registry's token0/token1.
    Registry,
    /// Bound by the nearest before/after `Transfer` with the pool as counterparty.
    Transfer,
    /// ±24-log window, or a leg whose tokens were not resolved by either of the
    /// trusted paths (including Balancer topic tokens, which skip pairing).
    Proximity,
}

impl LegSource {
    pub fn as_str(self) -> &'static str {
        match self {
            LegSource::Registry => "registry",
            LegSource::Transfer => "transfer",
            LegSource::Proximity => "proximity",
        }
    }
}

/// A decoded DEX swap fact from a receipt log. Token direction is resolved
/// via the pool registry, then adjacent ERC-20 Transfer legs. `token_source`
/// records which path actually bound the tokens so pricing can ignore guesses.
#[derive(Debug, Clone)]
pub struct SwapFact {
    pub tx_index: u64,
    pub log_index: u64,
    pub pool: Address,
    pub amm: Amm,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_out: U256,
    /// Provenance of `token_in`/`token_out`. Defaults to [`LegSource::Proximity`]
    /// until `attach_swap_tokens` resolves the leg.
    pub token_source: LegSource,
    /// Post-swap pool tick for concentrated-liquidity AMMs (V3/V4/Infinity),
    /// used to validate JIT tick-range overlap. `None` for V2/Curve/Balancer.
    pub tick: Option<i32>,
    /// Flow-ownership attribution (§7.1/§8.1): the address that funded the
    /// swap's input leg (the `from` of the nearest inbound transfer to the
    /// pool before the swap log), when observable from the transfer stream.
    /// `None` when no inbound leg is attributable. Transient — not persisted.
    pub owner: Option<Address>,
}

/// A decoded flash-loan fact (Aave V2/V3, Balancer V2, Uni V3).
#[derive(Debug, Clone)]
pub struct FlashLoanFact {
    pub tx_index: u64,
    pub log_index: u64,
    pub protocol: &'static str,
    /// Borrower/initiator of the loan.
    pub initiator: Address,
    pub token: Address,
    pub amount: U256,
    /// Fee in `token` units when the event carries it (Aave). Balancer/Uni V3
    /// fees are computed per-pool and are `None` here.
    pub fee: Option<U256>,
    /// Recipient of the loan (V3/Uni V3); used for netting attribution.
    pub recipient: Address,
    /// Emitting provider contract (Aave pool / Balancer vault); used to detect
    /// whether the repay leg is already present in the transfer stream.
    pub provider: Address,
}

/// A decoded lending-protocol liquidation fact.
#[derive(Debug, Clone)]
pub struct LiquidationFact {
    pub tx_index: u64,
    pub log_index: u64,
    pub protocol: &'static str,
    /// Emitting contract (pool / market / vault). Used to remap emitters that
    /// share a topic0 with another protocol (Spark, Benqi) via `ChainConfig`
    /// without new code.
    pub emitter: Address,
    pub user: Address,
    pub liquidator: Address,
    pub collateral_asset: Address,
    pub debt_asset: Address,
    pub collateral_amount: U256,
    pub debt_to_cover: U256,
    /// Morpho Blue `badDebtAssets` (plan P3.14); zero when the event has none.
    pub bad_debt_assets: U256,
}

/// Compound V3 Comet `BuyCollateral` — discount capture after Absorb (§26).
#[derive(Debug, Clone)]
pub struct BuyCollateralFact {
    pub tx_index: u64,
    pub log_index: u64,
    pub protocol: &'static str,
    pub emitter: Address,
    pub buyer: Address,
    pub collateral_asset: Address,
    /// Base-token amount paid (coins).
    pub base_amount: U256,
    /// Collateral amount received.
    pub collateral_amount: U256,
}

/// Realized P&L valuation basis (§0.1 of explorer_strategy_tracking_plan).
/// Stored on every classified instance as `details.pnl_basis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PnlBasis {
    /// Realized in-tx net token deltas (arb, sandwich, skim, flash-arb).
    R,
    /// Oracle-valued (liquidations, auction takes, discount captures).
    O,
    /// Explicit fee/reward event fields (keeper, flash premium, JIT tip).
    F,
}

impl PnlBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            PnlBasis::R => "R",
            PnlBasis::O => "O",
            PnlBasis::F => "F",
        }
    }
}

/// Chainlink AggregatorV3 `AnswerUpdated` (explorer plan P1.4).
#[derive(Debug, Clone)]
pub struct OracleUpdateFact {
    pub tx_index: u64,
    pub log_index: u64,
    /// Aggregator proxy / feed contract that emitted the update.
    pub feed: Address,
    /// Indexed `current` answer (raw aggregator units) for mode-B pre-poke
    /// divergence vs the prior stored answer.
    pub answer: i128,
}

/// Balancer ComposableStablePool `TokenRateCacheUpdated` (plan P3.2).
#[derive(Debug, Clone)]
pub struct RateCacheFact {
    pub tx_index: u64,
    pub log_index: u64,
    /// ComposableStablePool that refreshed its cached rate.
    pub pool: Address,
    pub token_index: u64,
    pub rate: U256,
}

/// Aave-family `ReserveDataUpdated` (explorer plan P1.1 interest attribution).
#[derive(Debug, Clone)]
pub struct ReserveDataFact {
    pub tx_index: u64,
    pub log_index: u64,
    pub reserve: Address,
    pub variable_borrow_rate: U256,
}

/// Keeper / automation execution (Gelato Automate, Chainlink Automation —
/// explorer plan P1.5 / §21).
#[derive(Debug, Clone)]
pub struct KeeperFact {
    pub tx_index: u64,
    pub log_index: u64,
    pub protocol: &'static str,
    /// Emitting registry / Automate contract.
    pub emitter: Address,
    /// Fee paid to the keeper network when the event carries it.
    pub fee: Option<U256>,
    pub fee_token: Option<Address>,
}

/// ve(3,3) gauge `NotifyReward` — epoch emission / bribe signal (P3.15).
#[derive(Debug, Clone)]
pub struct EpochRewardFact {
    pub tx_index: u64,
    pub log_index: u64,
    pub emitter: Address,
    pub reward_token: Address,
    pub amount: U256,
}

/// GMX V2 EventEmitter ADL / liquidation-adjacent log (P3.7).
#[derive(Debug, Clone)]
pub struct GmxEventFact {
    pub tx_index: u64,
    pub log_index: u64,
    pub emitter: Address,
    /// `"adl"` | `"liquidation"` | `"impact"`.
    pub kind: &'static str,
}

/// ERC-4337 EntryPoint `UserOperationEvent` (P3.11).
#[derive(Debug, Clone)]
pub struct UserOpFact {
    pub tx_index: u64,
    pub log_index: u64,
    pub entry_point: Address,
    pub sender: Address,
    pub paymaster: Address,
    pub actual_gas_cost: U256,
    pub success: bool,
}

/// A decoded V3 concentrated-liquidity Mint/Burn fact, or an LFJ/Pharaoh
/// Liquidity Book DepositedToBins/WithdrawnFromBins fact (bin ids mapped into
/// `tick_lower`/`tick_upper` as the inclusive min/max bin id).
#[derive(Debug, Clone)]
pub struct JitFact {
    pub tx_index: u64,
    pub log_index: u64,
    pub pool: Address,
    pub owner: Address,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub is_mint: bool,
    pub liquidity: u128,
    pub amount0: U256,
    pub amount1: U256,
    /// When true, overlap is any same-pool swap (LB has no tick on Swap).
    pub bin_amm: bool,
}

/// UniV2 pair lifecycle op used to exclude Swap/Mint/Burn/Sync outflows from
/// skim classification (skim itself emits Transfers only — never Sync).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V2PairOpKind {
    Sync,
    Mint,
    Burn,
}

/// A decoded UniV2-style Sync/Mint/Burn log (pool address + kind only).
#[derive(Debug, Clone)]
pub struct V2PairOpFact {
    pub tx_index: u64,
    pub log_index: u64,
    pub pool: Address,
    pub kind: V2PairOpKind,
}
