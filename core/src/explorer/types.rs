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
            "unknown" => Some(MevKind::Unknown),
            _ => None,
        }
    }

    /// Live-feed default kinds (Phase 3 ship gate): exclude `Frontrun` /
    /// `Backrun` until the labeled causal golden set is accepted for
    /// production. Pass `--kinds all` (CLI) or include them explicitly to
    /// opt in. Classification still emits both kinds into `mev_ops`.
    /// Config override: `[explorer].live_feed_kinds` (comma-separated).
    pub fn live_feed_default_kinds() -> Vec<MevKind> {
        vec![
            MevKind::ArbAtomic,
            MevKind::Sandwich,
            MevKind::Liquidation,
            MevKind::Jit,
            MevKind::JitArb,
            MevKind::Unknown,
        ]
    }

    /// Parse a comma-separated kinds list for live-feed config/CLI.
    /// `"all"` → empty vec (caller treats as no filter / every kind).
    pub fn parse_kinds_list(s: &str) -> Option<Vec<MevKind>> {
        if s.eq_ignore_ascii_case("all") {
            return Some(vec![]);
        }
        let mut out = Vec::new();
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            out.push(MevKind::parse(part)?);
        }
        Some(out)
    }
}

impl std::fmt::Display for MevKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleLeg {
    pub tx_index: u64,
    pub tx_hash: B256,
    /// front_run | victim | back_run
    pub role: String,
    pub pool: Address,
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

/// A decoded DEX swap fact from a receipt log. Token direction is resolved
/// via adjacent ERC-20 Transfer legs (registry-free, chain-generic).
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
    pub user: Address,
    pub liquidator: Address,
    pub collateral_asset: Address,
    pub debt_asset: Address,
    pub collateral_amount: U256,
    pub debt_to_cover: U256,
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
