//! Realized-MEV explorer: forensic reconstruction of MEV actually extracted
//! on-chain, from raw block/receipt data via RPC (logs-only, no traces in the
//! backfill path).
//!
//! Conceptual split vs the rest of the crate: `mev::` detectors answer "what
//! could be made" (opportunity simulation); `explorer::` answers "what was
//! made" (pattern classification over realized transactions + profit
//! attribution from token balance deltas).
//!
//! Pipeline: `ingest` (block+receipt fetch, reorg-aware) → `decode` (swaps,
//! transfers, liquidation facts) → `classify` (per-block pattern passes) →
//! `profit` (balance-delta accounting, gas, USD) → `store` (SQLite facts) →
//! `query` (live/stats/top/show/export CLI surface).
//!
//! Classification passes (in `classify`), applied per block in priority order:
//! atomic-arbitrage cycles (`ArbAtomic`, exact/estimated), three-leg sandwiches
//! (`Sandwich`), frontruns (`Frontrun`), backruns (`Backrun`), and narrowly
//!-intersected JIT liquidity (`Jit`). Kinds are mutually exclusive by priority
//! (`Sandwich` > `Frontrun` > `Backrun` > `ArbAtomic` > `JitArb` > `Jit`).
//! Costs are netted per op: gas (wei × effective price → USD via native price)
//! and any flash-loan fee (USD via borrowed token price); a sandwich whose net
//! is non-positive is dropped at persist time (Phase 1.4 profitability gate).
//! The atomic-arb pass (Phase 1.2) requires the closed-cycle swaps to belong to
//! a single funder-owner that is a searcher candidate of that transaction
//! (`exact`) unless the cycle is unowned (`estimated`); it is otherwise skipped
//! to control false positives.

pub mod canonical;
pub mod classify;
pub mod decode;
pub mod golden;
pub mod ingest;
pub mod pricing;
pub mod profit;
pub mod reject;
pub mod results;
pub mod store;
pub mod types;
pub mod validate;

pub use canonical::explorer_canonical_id;
pub use classify::classify_block;
pub use golden::{score_embedded_causal_set, GoldenSetScore};
pub use reject::{RejectReason, RejectedCandidate};
pub use results::{persist_opportunities_to_explorer, persist_rejections_to_explorer};
pub use types::{Confidence, MevBundle, MevEvent, MevKind};
