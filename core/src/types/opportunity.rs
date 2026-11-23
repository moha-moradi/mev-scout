//! Data contracts for detected MEV opportunities and persisted result files.
//!
//! These types are the serialization boundary between the core backtest engine,
//! the CLI output layer, and the API serialization layer.
use crate::explorer::profit::{compute_net_profit, ProfitCosts};
use crate::types::strategy::Strategy;
use alloy::primitives::{keccak256, Address, B256, U256};
use serde::{Deserialize, Serialize};

/// How an opportunity was detected — stable string at the DB/JSON boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionPath {
    /// Full EVM replay / backtest (`run`).
    Replay,
    /// Pending-mempool capture.
    Pending,
    /// Log-only synthesis (`live` without full replay).
    LogOnly,
}

impl DetectionPath {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Replay => "replay",
            Self::Pending => "pending",
            Self::LogOnly => "log_only",
        }
    }
}

impl std::fmt::Display for DetectionPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for DetectionPath {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "replay" => Ok(Self::Replay),
            "pending" => Ok(Self::Pending),
            "log_only" => Ok(Self::LogOnly),
            other => Err(format!("unknown detection_path '{other}'")),
        }
    }
}

/// A detected MEV opportunity from backtesting.
///
/// Different strategies populate different optional fields:
/// - `path` for multi-hop strategies,
/// - `tick_lower`/`tick_upper`/`liquidity_amount` for JIT strategies.
///
/// For `Strategy::Backrun`, `sender` / `tx_hash` / `tx_index` denote the
/// **anchor** (victim) transaction, not a searcher transaction;
/// `victim_tx_index` carries the same index semantically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevOpportunity {
    /// Canonical dedup ID: derived from strategy + key fields to uniquely
    /// identify this opportunity across detectors and aggregation passes.
    /// Example: "TwoHopArb|0xaaa|0xbbb|0xccc|0xddd".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_id: Option<String>,
    /// Block where the opportunity was detected
    pub block_number: u64,
    /// Index of the transaction after which the opportunity exists
    pub tx_index: usize,
    /// The strategy type
    pub strategy: Strategy,
    /// Pool involved in the first swap
    pub pool_a: Address,
    /// Pool involved in the second swap
    pub pool_b: Address,
    /// Token being arbitraged (input token)
    pub token_in: Address,
    /// Token received as output
    pub token_out: Address,
    /// Amount of token_in to invest
    pub input_amount: U256,
    /// Expected profit in token_out (gross, before gas)
    pub expected_profit: U256,
    /// Raw profit in token_out before normalization to native (None = same as expected_profit)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_profit: Option<U256>,
    /// Profit estimate with +1% slippage (more input) — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit_slippage_p1: Option<U256>,
    /// Profit estimate with -1% slippage (less input) — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit_slippage_m1: Option<U256>,
    /// Profit estimate with +2% slippage — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit_slippage_p2: Option<U256>,
    /// Profit estimate with -2% slippage — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit_slippage_m2: Option<U256>,
    /// Estimated gas cost in wei
    pub gas_cost_wei: u128,
    /// Timestamp of the block
    pub timestamp: u64,
    /// Full pool path for multi-hop opportunities (e.g., [buy, intermediate,..., sell])
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<Vec<Address>>,
    /// Tick range lower bound (JIT liquidity positions)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_lower: Option<i32>,
    /// Tick range upper bound (JIT liquidity positions)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_upper: Option<i32>,
    /// Amount of liquidity deployed (JIT positions)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liquidity_amount: Option<u128>,
    /// Transaction index of the victim swap. For `Strategy::Backrun`, this
    /// identifies the anchor transaction that triggered the backrun opportunity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub victim_tx_index: Option<usize>,
    /// Transaction index of the backrun. For regular backtests, this is typically
    /// `None` (backrun candidates are not observed as realized searcher txs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backrun_tx_index: Option<usize>,
    /// Whether this opportunity was detected from mempool/pending transactions
    /// rather than settled on-chain blocks.
    #[serde(default)]
    pub mempool_only: bool,
    /// Confidence score (0.0–1.0) for speculative detection methods.
    /// None = standard on-chain detected opportunity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Transaction sender (EOA `from`) when known. Populated by the runner
    /// stamping pass; enables sender-based joins and explorer sender metrics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<Address>,
    /// On-chain transaction hash when the opportunity is anchored to a
    /// specific transaction (replay path). Enables tx-granularity matching
    /// against realized explorer ops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<B256>,
    /// How this opportunity was detected (`replay` / `log_only` / `pending`).
    /// Makes run-vs-live coverage gaps attributable to detection path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_path: Option<DetectionPath>,
    /// Attribution tags shared with the explorer (`backrun`, `flash_arb`,
    /// `long_tail`, …). Empty for strategies that carry no sub-tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Exact token order of the route: `len == path.len() + 1`.
    /// Multi-token Curve/Balancer pools make the hop token ambiguous from the
    /// pool set alone, so sim/exec must not re-derive it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_tokens: Option<Vec<Address>>,
    /// Virtual Benqi mint/redeem leg for `savax_rate_arb` rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub savax_route: Option<SavaxRoute>,
    /// Structured tag evidence (route shape, rate values, signal tx index).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,

    // --- Capital-free atomic-arb contract (WP0) ---------------------------
    // Every field below is additive: old results files omit them and deserialize
    // with the serde defaults. None of them changes the existing detector logic.
    /// Unix-ms deadline for this quote. A live gate must reject execution past it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_expires_at: Option<u64>,
    /// Hash of the underlying pool set behind an aggregator-wrapped route, so
    /// two wrappers over the same pools can be deduped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlying_pools_hash: Option<B256>,
    /// Router/executor version that produced the quote; two versions of the
    /// same route can have different P&L.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_version: Option<String>,
    /// Block whose state produced every state-relative field below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sim_block: Option<u64>,
    /// Base fee in wei observed at `sim_block`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basefee_wei: Option<u128>,
    /// Priority fee in wei at `sim_block`. Avalanche C-Chain has no PBS — this
    /// is a real `priority_fee`, never a `BuilderTip`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority_fee_wei: Option<u128>,
    /// Searcher nonce at `sim_block`, for nonce-conflict gating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<u64>,
    /// Searcher escrow balance in wei at `sim_block`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance_wei: Option<U256>,
    /// Token allowance in wei at `sim_block` (non-flash path only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowance_wei: Option<U256>,
    /// Risk flags raised by the live gate. A vector replaces a single boolean
    /// so unrelated blockers can be reported together.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risk_flags: Vec<String>,
    /// Optimal input `L*` returned by the amount optimizer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimal_amount: Option<U256>,
    /// Native wei burned on failed attempts while realizing this opportunity.
    /// Kept separate from `gas_cost_wei` so it is subtracted exactly once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert_gas_cost_wei: Option<u128>,
    /// Funding layer for this opportunity (`balancer_v2_vault`, `aave_v3`,
    /// `univ3_flash`, `lfj_lb_flash`, `v2_flash_swap`, …). This is a tag on the
    /// opportunity, not a separate arbitrage family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub funding_source: Option<String>,
    /// Explicit native net profit. `None` means derive it with
    /// [`Self::resolved_net_profit_wei`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_profit_wei: Option<U256>,
    /// Extension point for builder bids. Avalanche C-Chain has no PBS, so the
    /// only meaningful current value is JSON `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder_bid: Option<serde_json::Value>,
}

/// Direction of the virtual sAVAX leg in a rate-arb route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavaxDirection {
    /// Buy sAVAX in a pool, redeem for AVAX at the protocol rate.
    Redeem,
    /// Mint sAVAX with AVAX at the protocol rate, sell into a pool.
    Mint,
}

impl SavaxDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Redeem => "redeem",
            Self::Mint => "mint",
        }
    }
}

/// Virtual Benqi leg attached to an sAVAX rate-arb opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavaxRoute {
    /// AMM pool used for the market leg.
    pub pool: Address,
    pub direction: SavaxDirection,
    /// `getPooledAvaxByShares(1e18)` at detect time (AVAX wad per sAVAX).
    pub rate_wad: U256,
    /// sAVAX shares moved through the virtual leg.
    pub shares: U256,
    /// Minimum AVAX out (redeem) or sAVAX out (mint) after the rate leg.
    pub min_out_wad: U256,
}

/// sAVAX canonical id: mint and redeem in the same pool/block must not collapse.
pub fn compute_savax_canonical_id(pool: Address, direction: SavaxDirection) -> String {
    format!("SavaxRateArb|{pool:#x}|{}", direction.as_str())
}

/// Key fields used to build a canonical opportunity dedup id.
#[derive(Debug, Clone)]
pub struct CanonicalIdParts {
    pub strategy: Strategy,
    pub pool_a: Address,
    pub pool_b: Address,
    pub token_in: Address,
    pub token_out: Address,
    /// Underlying pool set behind an aggregator wrapper (M-06).
    pub underlying_pools_hash: Option<B256>,
    /// Router/executor version that produced the quote.
    pub router_version: Option<String>,
    /// Funding layer used by the route.
    pub funding_source: Option<String>,
}

/// Hash of the underlying pool set behind a route (M-06 / WP5).
///
/// Addresses are sorted and deduplicated before hashing so a route walked in
/// the opposite direction (or a duplicate log) produces the same value on the
/// simulated and realized sides. Aggregator/router edges must be excluded by
/// the caller: a wrapper's address changes with the routing layer, not with
/// the underlying liquidity the opportunity actually used.
pub fn underlying_pools_hash(pools: &[Address]) -> B256 {
    let mut sorted: Vec<Address> = pools.to_vec();
    sorted.sort();
    sorted.dedup();
    let mut bytes = Vec::with_capacity(sorted.len() * 20);
    for pool in sorted {
        bytes.extend_from_slice(pool.as_slice());
    }
    keccak256(bytes)
}

/// Deterministic canonical suffix for route/provenance refinements (M-06).
///
/// Segments are appended in a fixed order and only when present, so every
/// legacy canonical ID stays byte-identical. Both the simulated opportunity
/// side and the realized explorer side call this helper to avoid drift.
pub fn canonical_route_suffix(
    underlying_pools_hash: Option<B256>,
    router_version: Option<&str>,
    funding_source: Option<&str>,
) -> String {
    let mut suffix = String::new();
    if let Some(hash) = underlying_pools_hash {
        suffix.push_str(&format!("|h:{hash:#x}"));
    }
    if let Some(version) = router_version.filter(|s| !s.is_empty()) {
        suffix.push_str(&format!("|r:{version}"));
    }
    if let Some(source) = funding_source.filter(|s| !s.is_empty()) {
        suffix.push_str(&format!("|f:{source}"));
    }
    suffix
}

/// Build a canonical dedup string from the opportunity's key fields.
pub fn compute_canonical_id(parts: CanonicalIdParts) -> String {
    let mut id = format!(
        "{:?}|{:#x}|{:#x}|{:#x}|{:#x}",
        parts.strategy, parts.pool_a, parts.pool_b, parts.token_in, parts.token_out,
    );
    id.push_str(&canonical_route_suffix(
        parts.underlying_pools_hash,
        parts.router_version.as_deref(),
        parts.funding_source.as_deref(),
    ));
    id
}

/// Build a canonical id for backrun opportunities that matches the explorer's form.
pub fn compute_backrun_canonical_id(anchor_pool: Address, victim_tx_index: usize) -> String {
    format!("Backrun|{anchor_pool:#x}|source_tx:{victim_tx_index}")
}

impl MevOpportunity {
    /// Create a new MEV opportunity with required fields.
    /// Strategy-specific fields should be set via builder methods.
    pub fn new(
        block_number: u64,
        tx_index: usize,
        strategy: Strategy,
        pool_a: Address,
        timestamp: u64,
    ) -> Self {
        MevOpportunity {
            canonical_id: None,
            block_number,
            tx_index,
            strategy,
            pool_a,
            pool_b: Address::ZERO,
            token_in: Address::ZERO,
            token_out: Address::ZERO,
            input_amount: U256::ZERO,
            expected_profit: U256::ZERO,
            raw_profit: None,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            gas_cost_wei: 0,
            timestamp,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
            mempool_only: false,
            confidence: None,
            sender: None,
            tx_hash: None,
            detection_path: None,
            tags: Vec::new(),
            route_tokens: None,
            savax_route: None,
            details: None,
            quote_expires_at: None,
            underlying_pools_hash: None,
            router_version: None,
            sim_block: None,
            basefee_wei: None,
            priority_fee_wei: None,
            nonce: None,
            balance_wei: None,
            allowance_wei: None,
            risk_flags: Vec::new(),
            optimal_amount: None,
            revert_gas_cost_wei: None,
            funding_source: None,
            net_profit_wei: None,
            builder_bid: None,
        }
    }

    /// Net native profit, explicit when stamped by the ledger, otherwise
    /// derived from the fields we already have.
    ///
    /// `expected_profit` is the **gross** residual after the full simulated
    /// route, so swap fees and realized slippage are already inside it and are
    /// deliberately NOT subtracted again. Only gas and failed-attempt gas are
    /// external costs. A flash-loan fee, when present, is already netted into
    /// the simulated residual (see `paper::sim`), so it is not subtracted here.
    pub fn resolved_net_profit_wei(&self) -> U256 {
        if let Some(v) = self.net_profit_wei {
            return v;
        }
        // C-05: one canonical formula for every unsigned net-profit figure.
        // `expected_profit` is the full-route simulated residual, which already
        // has the flash premium netted (`paper::sim`), so the flash term is
        // deliberately zero here — subtracting it again would double-count.
        compute_net_profit(
            self.expected_profit,
            ProfitCosts {
                gas_wei: U256::from(self.gas_cost_wei),
                flashloan_fee_wei: U256::ZERO,
                revert_gas_wei: U256::from(self.revert_gas_cost_wei.unwrap_or(0)),
            },
        )
    }

    /// Add an attribution tag once (order preserved).
    pub fn add_tag(&mut self, tag: &str) {
        if !self.tags.iter().any(|t| t == tag) {
            self.tags.push(tag.to_string());
        }
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }
}

/// Saved results file wrapping opportunities with run metadata.
///
/// Built during `run`/`live` for persistence into the explorer SQLite store,
/// and reconstructed by the `report` subcommand from stored rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultsFile {
    pub run_id: String,
    pub chain: String,
    pub start_block: u64,
    pub end_block: u64,
    pub range_mode: String,
    pub strategies: Vec<String>,
    pub flash_loan_provider: String,
    pub resolved_at: u64,
    pub created_at: u64,
    pub opportunities: Vec<MevOpportunity>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A legacy results row (no WP0 fields) must still deserialize, and a new
    /// row must survive a serde round-trip with every additive field intact.
    #[test]
    fn opportunity_contract_is_backward_compatible_and_round_trips() {
        let legacy = r#"{
            "block_number": 1, "tx_index": 0, "strategy": "TwoHopArb",
            "pool_a": "0x0000000000000000000000000000000000000001",
            "pool_b": "0x0000000000000000000000000000000000000002",
            "token_in": "0x0000000000000000000000000000000000000003",
            "token_out": "0x0000000000000000000000000000000000000004",
            "input_amount": "100", "expected_profit": "10", "gas_cost_wei": 3,
            "timestamp": 1
        }"#;
        let old: MevOpportunity = serde_json::from_str(legacy).expect("legacy row");
        assert!(old.funding_source.is_none());
        assert!(old.risk_flags.is_empty());
        assert!(old.optimal_amount.is_none());
        assert_eq!(old.resolved_net_profit_wei(), U256::from(7));

        let mut opp = MevOpportunity::new(2, 1, Strategy::TwoHopArb, Address::ZERO, 2);
        opp.expected_profit = U256::from(100);
        opp.gas_cost_wei = 10;
        opp.revert_gas_cost_wei = Some(3);
        opp.funding_source = Some("balancer_v2_vault".into());
        opp.sim_block = Some(9);
        opp.basefee_wei = Some(7);
        opp.priority_fee_wei = Some(2);
        opp.risk_flags.push("quote_drift".into());
        opp.quote_expires_at = Some(1234);
        opp.net_profit_wei = Some(U256::from(85));
        opp.optimal_amount = Some(U256::from(123));
        opp.builder_bid = Some(serde_json::Value::Null);
        let json = serde_json::to_string(&opp).expect("serialize");
        let back: MevOpportunity = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.funding_source.as_deref(), Some("balancer_v2_vault"));
        assert_eq!(back.sim_block, Some(9));
        assert_eq!(back.risk_flags, vec!["quote_drift"]);
        assert_eq!(back.quote_expires_at, Some(1234));
        assert_eq!(back.net_profit_wei, Some(U256::from(85)));
        assert_eq!(back.optimal_amount, Some(U256::from(123)));
        assert_eq!(back.resolved_net_profit_wei(), U256::from(85));
        // Without an explicit net field the derived value subtracts gas +
        // revert gas exactly once (100 - 10 - 3 = 87).
        let mut derived = back;
        derived.net_profit_wei = None;
        assert_eq!(derived.resolved_net_profit_wei(), U256::from(87));
    }

    /// C-05: the persisted/derived net figure and the plan's canonical formula
    /// must be the same function. A flash premium is never subtracted twice.
    #[test]
    fn resolved_net_profit_uses_canonical_formula() {
        let mut opp = MevOpportunity::new(1, 0, Strategy::TwoHopArb, Address::ZERO, 1);
        opp.expected_profit = U256::from(100);
        opp.gas_cost_wei = 10;
        opp.revert_gas_cost_wei = Some(3);
        let expected = compute_net_profit(
            opp.expected_profit,
            ProfitCosts {
                gas_wei: U256::from(10),
                flashloan_fee_wei: U256::ZERO,
                revert_gas_wei: U256::from(3),
            },
        );
        assert_eq!(expected, U256::from(87));
        assert_eq!(opp.resolved_net_profit_wei(), expected);
    }

    /// M-06: routers, underlying pool sets and funding sources that share the
    /// same endpoint pools must not collapse into one canonical row. Legacy
    /// callers omit all three and keep the old ID byte-for-byte.
    #[test]
    fn canonical_id_separates_router_pool_hash_and_funding() {
        let base = || CanonicalIdParts {
            strategy: Strategy::TwoHopArb,
            pool_a: Address::ZERO,
            pool_b: Address::ZERO,
            token_in: Address::ZERO,
            token_out: Address::ZERO,
            underlying_pools_hash: None,
            router_version: None,
            funding_source: None,
        };
        let zero = "0x0000000000000000000000000000000000000000";
        assert_eq!(
            compute_canonical_id(base()),
            format!("TwoHopArb|{zero}|{zero}|{zero}|{zero}")
        );

        let mut v1 = base();
        v1.router_version = Some("v1".into());
        let mut v2 = base();
        v2.router_version = Some("v2".into());
        assert_ne!(compute_canonical_id(v1.clone()), compute_canonical_id(v2));
        assert!(compute_canonical_id(v1).ends_with("|r:v1"));

        let h1 = "0x1111111111111111111111111111111111111111111111111111111111111111"
            .parse::<B256>()
            .unwrap();
        let h2 = "0x2222222222222222222222222222222222222222222222222222222222222222"
            .parse::<B256>()
            .unwrap();
        let mut a = base();
        a.underlying_pools_hash = Some(h1);
        let mut b = base();
        b.underlying_pools_hash = Some(h2);
        assert_ne!(compute_canonical_id(a), compute_canonical_id(b));

        let mut f1 = base();
        f1.funding_source = Some("balancer_v2_vault".into());
        let mut f2 = base();
        f2.funding_source = Some("aave_v3".into());
        assert_ne!(compute_canonical_id(f1), compute_canonical_id(f2));

        // Fixed segment order independent of field insertion order.
        let mut all = base();
        all.underlying_pools_hash = Some(h1);
        all.router_version = Some("v1".into());
        all.funding_source = Some("aave_v3".into());
        assert!(compute_canonical_id(all).ends_with(&format!("|h:{h1:#x}|r:v1|f:aave_v3")));
    }

    #[test]
    fn underlying_pool_hash_is_order_and_duplicate_invariant() {
        let a = Address::repeat_byte(0x11);
        let b = Address::repeat_byte(0x22);
        assert_eq!(
            underlying_pools_hash(&[a, b]),
            underlying_pools_hash(&[b, a])
        );
        assert_eq!(
            underlying_pools_hash(&[a, b]),
            underlying_pools_hash(&[b, a, b])
        );
        assert_ne!(underlying_pools_hash(&[a]), underlying_pools_hash(&[b]));
    }
}
