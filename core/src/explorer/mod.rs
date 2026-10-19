//! Realized-MEV explorer: forensic reconstruction of MEV actually extracted
//! on-chain, from raw block/receipt data via RPC (logs-only, no traces in the
//! live index path).
//!
//! Conceptual split vs the rest of the crate: `mev::` detectors answer "what
//! could be made" (opportunity simulation); `explorer::` answers "what was
//! made" (pattern classification over realized transactions + profit
//! attribution from token balance deltas).
//!
//! Pipeline: `ingest` (block+receipt fetch, reorg-aware) → `decode` (swaps,
//! transfers, liquidation facts) → `classify` (per-block pattern passes) →
//! `profit` (balance-delta accounting, gas, USD) → `store` (SQLite facts) →
//! `query` (revenue-report / index CLI surface).
//!
//! Classification passes (in `classify`), applied per block in priority order:
//! liquidations, UniV2 `skim()` capture (`Skim`), atomic-arbitrage cycles
//! (`ArbAtomic`, Exact only), three-leg sandwiches (`Sandwich`), frontruns
//! (`Frontrun`), backruns (`Backrun`), and narrowly-intersected JIT liquidity
//! (`Jit`). Kinds are mutually exclusive by priority (`Sandwich` > `Frontrun` >
//! `Backrun` > `ArbAtomic` > `JitArb` > `Jit` > `Liquidation` > `Skim`).
//! Costs are netted per op: gas (wei × effective price → USD via native price)
//! and any flash-loan fee (USD via borrowed token price); a sandwich whose net
//! is non-positive is dropped at persist time (profitability gate).
//! The atomic-arb pass requires a closed multi-pool swap cycle with flow
//! ownership collapsing to a single searcher candidate and a closed ledger
//! (no open inventory spend); otherwise the tx is skipped.

pub(crate) mod benqi_liq;
pub(crate) mod canonical;
pub(crate) mod classify;
pub(crate) mod decode;
#[cfg(test)]
pub(crate) mod golden;
pub mod ingest;
pub(crate) mod interest_attr;
pub(crate) mod pricing;
pub mod profit;
pub(crate) mod reject;
pub(crate) mod results;
pub mod scenario_targets;
#[cfg(test)]
mod scenarios;
pub mod store;
pub(crate) mod strategy_tags;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub mod types;

pub use canonical::explorer_canonical_id;
pub use classify::{classify_block, BlockInput, TxInput};
pub use pricing::TokenUsd;
pub use reject::{RejectReason, RejectedCandidate};
pub use results::{persist_opportunities_to_explorer, persist_rejections_to_explorer};
pub use types::{BundleLegRole, Confidence, LiquidationDetails, MevBundle, MevEvent, MevKind};
