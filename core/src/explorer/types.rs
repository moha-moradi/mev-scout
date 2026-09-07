//! Core data contracts for the realized-MEV explorer.
//!
//! `MevEvent` is one classified operation anchored to a single transaction
//! (arb, liquidation, JIT, unknown); `MevBundle` groups multi-transaction
//! operations (sandwich front-run + victim(s) + back-run).

use alloy::primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};

/// MEV pattern kinds in scope (taxonomy §3 of the unified plan).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MevKind {
    /// Single tx whose token-flow graph closes a cycle ending in the start token.
    ArbAtomic,
    /// Same sender opens + closes a position around a victim swap on one pool.
    Sandwich,
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
            "liquidation" => Some(MevKind::Liquidation),
            "jit" => Some(MevKind::Jit),
            "jit_arb" => Some(MevKind::JitArb),
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
    /// USD value of the profit when pricing was available.
    pub profit_usd: Option<f64>,
    /// Gas cost in wei (gasUsed × effectiveGasPrice from the receipt).
    pub gas_cost_wei: U256,
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
    Curve,
    Balancer,
    Solidly,
    Lb,
    Pendle,
}

impl Amm {
    pub fn as_str(self) -> &'static str {
        match self {
            Amm::V2 => "v2",
            Amm::V3 => "v3",
            Amm::V4 => "v4",
            Amm::Curve => "curve",
            Amm::Balancer => "balancer",
            Amm::Solidly => "solidly",
            Amm::Lb => "lb",
            Amm::Pendle => "pendle",
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

/// A decoded V3 concentrated-liquidity Mint/Burn fact.
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
}
